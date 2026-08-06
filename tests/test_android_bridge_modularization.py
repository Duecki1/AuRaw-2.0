from __future__ import annotations

import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
CLASS_ROOT = ROOT / "android/app/build/intermediates/javac/debug/compileDebugJavaWithJavac/classes"


def android_sdk() -> Path | None:
    configured = os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME")
    if configured:
        candidate = Path(configured)
        return candidate if candidate.is_dir() else None
    local = ROOT / "android/local.properties"
    if local.is_file():
        for line in local.read_text(encoding="utf-8").splitlines():
            if line.startswith("sdk.dir="):
                candidate = Path(line.split("=", 1)[1].replace("\\:", ":").replace("\\\\", "\\"))
                return candidate if candidate.is_dir() else None
    return None


@pytest.fixture(scope="module")
def compiled_android_classes() -> Path:
    if android_sdk() is None:
        pytest.skip("Android SDK is required for compiler-backed bridge ownership tests")
    completed = subprocess.run(
        [
            str(ROOT / "gradlew"),
            "-p",
            "android",
            "--no-daemon",
            "-PaurawBuildRust=false",
            ":app:compileDebugJavaWithJavac",
        ],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=600,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert CLASS_ROOT.is_dir()
    return CLASS_ROOT


def javap(class_root: Path, class_name: str) -> str:
    executable = shutil.which("javap")
    if executable is None:
        pytest.skip("javap is required to inspect compiler output")
    completed = subprocess.run(
        [executable, "-private", "-classpath", str(class_root), class_name],
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return completed.stdout


def declared_methods(bytecode_declaration: str) -> set[str]:
    return set(re.findall(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\([^;{}]*\);", bytecode_declaration))


def declared_field_types(bytecode_declaration: str) -> set[str]:
    return set(
        re.findall(
            r"^\s*(?:public|protected|private)?\s*(?:final\s+)?([A-Za-z0-9_.$]+)\s+[A-Za-z0-9_$]+;",
            bytecode_declaration,
            flags=re.MULTILINE,
        )
    )


def test_native_activity_compiles_as_a_thin_delegate_facade(compiled_android_classes: Path) -> None:
    activity = javap(compiled_android_classes, "de.duecki.auraw.AuRawActivity")
    methods = declared_methods(activity)
    fields = declared_field_types(activity)

    assert {
        "de.duecki.auraw.StorageManager",
        "de.duecki.auraw.ProfileImporter",
        "de.duecki.auraw.ExportPublisher",
    } <= fields
    assert {"openRawDocument", "openCameraProfileFolder", "onActivityResult"} <= methods
    assert {
        "listRawLibrary",
        "publishRawSidecar",
        "createPendingExport",
        "publishImage",
        "removeCameraProfileMirror",
    }.isdisjoint(methods)


def test_compiled_delegates_own_storage_profile_and_export_apis(
    compiled_android_classes: Path,
) -> None:
    storage = declared_methods(javap(compiled_android_classes, "de.duecki.auraw.StorageManager"))
    profiles = declared_methods(javap(compiled_android_classes, "de.duecki.auraw.ProfileImporter"))
    exports = declared_methods(javap(compiled_android_classes, "de.duecki.auraw.ExportPublisher"))

    assert {
        "listRawLibrary",
        "publishRawSidecar",
        "startLegacyRawStorageMigration",
        "handleRawDocumentResult",
    } <= storage
    assert {"createFolderPickerIntent", "removeCameraProfileMirror", "handleFolderPickerResult"} <= profiles
    assert {"createPendingExport", "publishImage", "onRequestPermissionsResult"} <= exports
