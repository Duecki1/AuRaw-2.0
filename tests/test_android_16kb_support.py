from __future__ import annotations

import json
import os
import subprocess
import tomllib
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO_MANIFEST = ROOT / "Cargo.toml"


def workspace_metadata() -> dict[str, object]:
    with CARGO_MANIFEST.open("rb") as handle:
        return tomllib.load(handle)["workspace"]["metadata"]


def command_contract(command: str) -> dict[str, object]:
    invocation = ["cargo", "xtask", command]
    if command != "print-metadata":
        invocation.append("--print-build-contract")
    completed = subprocess.run(
        invocation,
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return json.loads(completed.stdout)


def expected_contract() -> dict[str, object]:
    metadata = workspace_metadata()
    return {
        "ndkVersion": metadata["android_ndk_version"],
        "buildToolsVersion": metadata["android_build_tools_version"],
        "compileSdk": metadata["android_compile_sdk"],
        "minSdk": metadata["android_min_sdk"],
        "targetSdk": metadata["android_target_sdk"],
        "librawRevision": metadata["libraw_revision"],
        "lensfunRevision": metadata["lensfun_revision"],
        "useLegacyPackaging": metadata["android_use_legacy_packaging"],
    }


def test_android_build_tools_share_cargo_workspace_metadata() -> None:
    contract = expected_contract()
    assert int(str(contract["ndkVersion"]).split(".", 1)[0]) >= 28
    assert contract["useLegacyPackaging"] is False
    assert contract["minSdk"] <= contract["targetSdk"] <= contract["compileSdk"]
    for command in (
        "print-metadata",
        "build-android",
        "build-android-libraw",
        "build-android-lensfun",
        "verify-android-16kb",
    ):
        assert command_contract(command) == contract


def test_deprecated_properties_file_contains_no_contract_values() -> None:
    legacy = (ROOT / "android/build-contract.properties").read_text(encoding="utf-8")
    assert not any(
        line.strip() and not line.lstrip().startswith(("#", "!"))
        for line in legacy.splitlines()
    )


def test_gradle_and_rust_consume_cargo_workspace_metadata() -> None:
    gradle = (ROOT / "android/app/build.gradle").read_text(encoding="utf-8")
    assert 'new File(repositoryRoot, "Cargo.toml")' in gradle
    assert "build-contract.properties" not in gradle
    for wiring in (
        "compileSdk aurawCompileSdk",
        "buildToolsVersion aurawBuildToolsVersion",
        "ndkVersion aurawNdkVersion",
        "minSdk aurawMinSdk",
        "targetSdk aurawTargetSdk",
        "useLegacyPackaging aurawUseLegacyPackaging",
        'path file("src/main/cpp/CMakeLists.txt")',
        'targets "auraw_native_deps"',
    ):
        assert wiring in gradle

    build_script = (ROOT / "crates/auraw-core/build.rs").read_text(encoding="utf-8")
    helper = (
        ROOT / "crates/auraw-core/build_support/workspace_metadata.rs"
    ).read_text(encoding="utf-8")
    public_contract = (
        ROOT / "crates/auraw-core/src/build_metadata.rs"
    ).read_text(encoding="utf-8")
    assert "WorkspaceMetadata::load_from_manifest_dir" in build_script
    assert 'env::var_os("CARGO_MANIFEST_DIR")' in helper
    assert "cargo:rustc-env=AURAW_ANDROID_MIN_SDK" in helper
    assert 'env!("AURAW_LIBRAW_REVISION")' in public_contract


def fake_android_toolchain(tmp_path: Path, alignment_power: int) -> tuple[Path, Path, dict[str, str]]:
    contract = expected_contract()
    ndk_version = str(contract["ndkVersion"])
    build_tools_version = str(contract["buildToolsVersion"])
    sdk = tmp_path / "sdk"
    ndk = sdk / "ndk" / ndk_version
    host = ndk / "toolchains/llvm/prebuilt/linux-x86_64"
    objdump = host / "bin/llvm-objdump"
    objdump.parent.mkdir(parents=True)
    objdump.write_text(f"#!/bin/sh\necho '  LOAD off 0x0 align 2**{alignment_power}'\n", encoding="utf-8")
    objdump.chmod(0o755)
    (ndk / "source.properties").write_text(
        f"Pkg.Revision = {ndk_version}\n", encoding="utf-8"
    )
    zipalign = sdk / "build-tools" / build_tools_version / "zipalign"
    zipalign.parent.mkdir(parents=True)
    zipalign.write_text(
        "#!/bin/sh\n[ \"$1\" = -c ] && [ \"$2\" = -P ] && [ \"$3\" = 16 ]\n",
        encoding="utf-8",
    )
    zipalign.chmod(0o755)

    apk = tmp_path / "fixture.apk"
    with zipfile.ZipFile(apk, "w", compression=zipfile.ZIP_STORED) as archive:
        archive.writestr("lib/arm64-v8a/libauraw.so", b"ELF fixture")
    env = os.environ.copy()
    env["ANDROID_SDK_ROOT"] = str(sdk)
    env["ANDROID_NDK_HOME"] = str(ndk)
    return apk, sdk, env


def test_16kb_verifier_accepts_aligned_elf_and_apk(tmp_path: Path) -> None:
    apk, _, env = fake_android_toolchain(tmp_path, 14)
    completed = subprocess.run(
        ["cargo", "xtask", "verify-android-16kb", str(apk)],
        cwd=ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr


def test_16kb_verifier_rejects_under_aligned_elf(tmp_path: Path) -> None:
    apk, _, env = fake_android_toolchain(tmp_path, 13)
    completed = subprocess.run(
        ["cargo", "xtask", "verify-android-16kb", str(apk)],
        cwd=ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode != 0


def test_agp_cmake_builds_pinned_static_dependencies() -> None:
    gradle = (ROOT / "android/app/build.gradle").read_text(encoding="utf-8")
    cmake = (ROOT / "android/app/src/main/cpp/CMakeLists.txt").read_text(
        encoding="utf-8"
    )
    cargo_config = (ROOT / ".cargo/config.toml").read_text(encoding="utf-8")
    xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")

    assert 'path file("src/main/cpp/CMakeLists.txt")' in gradle
    assert 'targets "auraw_native_deps"' in gradle
    assert 'getOrElse("arm64-v8a,x86_64")' in gradle
    assert 'dependsOn("externalNativeBuildDebug")' in gradle
    assert 'environment "AURAW_NATIVE_DEPS_READY", "1"' in gradle

    for pin in (
        "b860248a89d9082b8e0a1e202e516f46af9adb29",
        "101c745e847a5de4a1e569a94368ce2027198598",
        "f5da1e522ea195b54b30f3ff105ef2193daa04ea165dea825b4d6fe9d886395b",
        "a11cbe6aeec657839540448b253217c25d20b7a45b6aebfef406f7239933c7a6",
    ):
        assert pin in cmake
    for option in (
        "-DBUILD_STATIC=ON",
        "-DBUILD_TESTS=OFF",
        "-DBUILD_LENSTOOL=OFF",
        "-DBUILD_DOC=OFF",
        "-DINSTALL_PYTHON_MODULE=OFF",
        "-DINSTALL_HELPER_SCRIPTS=OFF",
    ):
        assert option in cmake
    assert "add_subdirectory(" in cmake
    assert "ExternalProject_Add(auraw_lensfun" in cmake
    assert "add_library(auraw::native_static ALIAS auraw_native_static)" in cmake

    assert "max-page-size=16384" in cmake
    assert "common-page-size=16384" in cmake
    assert "[target.aarch64-linux-android]" in cargo_config
    assert "[target.x86_64-linux-android]" in cargo_config
    assert "max-page-size=16384" in cargo_config

    assert "run_gradle_android_native_dependencies" in xtask
    assert 'env::var_os("AURAW_NATIVE_DEPS_READY")' in xtask
    assert "python3 -m venv" not in xtask
