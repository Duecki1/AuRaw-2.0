from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
NDK_VERSION = "28.2.13676358"


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_android_uses_ndk_r28_or_newer_for_default_16kb_elf_alignment() -> None:
    gradle = read("android/app/build.gradle")
    rust_build = read("scripts/build-android.sh")
    libraw_build = read("scripts/build-android-libraw.sh")
    workflow = read(".gitea/workflows/build.yml")

    assert f'ndkVersion "{NDK_VERSION}"' in gradle
    assert f"EXPECTED_NDK_VERSION={NDK_VERSION}" in rust_build
    assert f"EXPECTED_NDK_VERSION={NDK_VERSION}" in libraw_build
    assert f'ANDROID_NDK_VERSION="{NDK_VERSION}"' in workflow
    assert tuple(map(int, NDK_VERSION.split(".")[:1])) >= (28,)


def test_android_packages_uncompressed_jni_libs_without_forced_extraction() -> None:
    gradle = read("android/app/build.gradle")
    manifest = read("android/app/src/main/AndroidManifest.xml")

    assert "packagingOptions" in gradle
    assert "jniLibs" in gradle
    assert "useLegacyPackaging false" in gradle
    assert "extractNativeLibs" not in manifest


def test_android_ci_verifies_16kb_elf_and_zip_alignment() -> None:
    verifier = read("scripts/verify-android-16kb.sh")
    workflow = read(".gitea/workflows/build.yml")

    assert "llvm-objdump" in verifier
    assert "arm64-v8a x86_64" in verifier
    assert "'$1 < 14" in verifier or "$1 < 14" in verifier
    assert '"$ZIPALIGN" -c -P 16 -v 4 "$APK"' in verifier
    assert 'sh scripts/verify-android-16kb.sh "$APK"' in workflow
