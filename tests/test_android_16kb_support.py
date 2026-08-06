from __future__ import annotations

import json
import os
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "android/build-contract.properties"


def read_properties(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            key, value = line.split("=", 1)
            result[key.strip()] = value.strip()
    return result


def command_contract(command: str) -> dict[str, object]:
    completed = subprocess.run(
        [sys.executable, "scripts/dev.py", command, "--print-build-contract"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return json.loads(completed.stdout)


def test_android_build_tools_share_one_executable_contract() -> None:
    properties = read_properties(CONTRACT_PATH)
    ndk = properties["ndkVersion"]
    assert int(ndk.split(".", 1)[0]) >= 28
    assert properties["useLegacyPackaging"].lower() == "false"
    for command in (
        "build-android",
        "build-android-libraw",
        "verify-android-16kb",
    ):
        assert command_contract(command)["ndkVersion"] == ndk
    assert (
        command_contract("verify-android-16kb")["buildToolsVersion"]
        == properties["buildToolsVersion"]
    )


def fake_android_toolchain(tmp_path: Path, alignment_power: int) -> tuple[Path, Path, dict[str, str]]:
    properties = read_properties(CONTRACT_PATH)
    sdk = tmp_path / "sdk"
    ndk = sdk / "ndk" / properties["ndkVersion"]
    host = ndk / "toolchains/llvm/prebuilt/linux-x86_64"
    objdump = host / "bin/llvm-objdump"
    objdump.parent.mkdir(parents=True)
    objdump.write_text(f"#!/bin/sh\necho '  LOAD off 0x0 align 2**{alignment_power}'\n", encoding="utf-8")
    objdump.chmod(0o755)
    (ndk / "source.properties").write_text(
        f"Pkg.Revision = {properties['ndkVersion']}\n", encoding="utf-8"
    )
    zipalign = sdk / "build-tools" / properties["buildToolsVersion"] / "zipalign"
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
        [sys.executable, "scripts/dev.py", "verify-android-16kb", str(apk)],
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
        [sys.executable, "scripts/dev.py", "verify-android-16kb", str(apk)],
        cwd=ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode != 0
