#!/usr/bin/env python3
"""Development and CI validation commands for AuRaw."""

from __future__ import annotations

import argparse
from collections.abc import Callable, Iterable, Sequence
import csv
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import ssl
import stat
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from typing import NoReturn
import urllib.error
import urllib.request
import zipfile

try:
    import numpy as np
except ModuleNotFoundError:  # Core validation commands use only the standard library.
    np = None  # type: ignore[assignment]

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"

GRADLE_PROPERTIES = ROOT / "gradle/wrapper/gradle-wrapper.properties"
GRADLE_WRAPPER_JAR = ROOT / "gradle/wrapper/gradle-wrapper.jar"
GRADLEW = ROOT / "gradlew"
GRADLEW_BAT = ROOT / "gradlew.bat"
EXPECTED_GRADLE_VERSION = "8.11.1"
EXPECTED_GRADLE_DISTRIBUTION_SHA256 = (
    "f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6"
)
EXPECTED_GRADLE_WRAPPER_JAR_SHA256 = (
    "2db75c40782f5e8ba1fc278a5574bab070adccb2d21ca5a6e5ed840888448046"
)

MODULE_RE = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PATH_MODULE_RE = re.compile(
    r'(?ms)#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]\s*'
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
INCLUDE_RE = re.compile(r'\binclude!\(\s*"([^"]+)"\s*\)\s*;')
DERIVE_RE = re.compile(r"#\s*\[\s*derive\b[^]]*\]")
WORKFLOW_USES_RE = re.compile(r"\buses:\s*([^\s#]+)")
COMMIT_SHA_RE = re.compile(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})")

BINARY_SUFFIXES = {
    ".a",
    ".aar",
    ".apk",
    ".class",
    ".dll",
    ".dylib",
    ".exe",
    ".jar",
    ".o",
    ".obj",
    ".rlib",
    ".rmeta",
    ".so",
}
ALLOWED_BINARY_PATHS = {"gradle/wrapper/gradle-wrapper.jar"}
IGNORED_BINARY_ROOTS = {
    ".git",
    ".gradle",
    "dist",
    "target",
    "android/.gradle",
    "android/build",
    "android/app/build",
    "android/native",
}

CAMERA_PROFILE_TEST_FILTERS = (
    "pipeline::color_profile::tests",
    "pipeline::color_profile::dcp::tests",
    "pipeline::color_profile::icc::tests",
    "pipeline::sigmoid::tests",
    "gpu_params_follow_the_wgsl_uniform_layout",
    "profile_shader_parses_with_the_profile_storage_contract",
    "adjustments_shader_exposes_darktable_sigmoid_paths",
    "scene_graph_preserves_native_call_order_and_stage_ownership",
    "global_wb_changes_raw_multipliers_without_changing_the_camera_transform",
)
DEMOSAIC_TEST_FILTERS = (
    "compute_shaders_parse_and_validate",
    "demosaic_contracts_are_compiler_validated",
    "demosaic_shaders_expose_every_dispatched_entry_point",
    "inpaint_opposed",
)
MATH_TEST_GROUPS = (
    ("camera profile", CAMERA_PROFILE_TEST_FILTERS),
    ("demosaic", DEMOSAIC_TEST_FILTERS),
)


@dataclass(frozen=True)
class CheckSpec:
    """A named validation function and its success message."""

    title: str
    success_message: str
    validate: Callable[[], list[str]]


def relative_display(path: Path) -> str:
    """Return a repository-relative path when possible."""
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return str(path)


def sha256(path: Path) -> str:
    """Calculate a file's SHA-256 digest without loading it all into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_properties(path: Path) -> dict[str, str]:
    """Parse the simple key=value format used by Gradle wrapper properties."""
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith(("#", "!")):
            continue
        if "=" not in line:
            raise ValueError(f"{relative_display(path)}:{line_number}: expected key=value")
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip()
    return values


def normalized_rust_version(version: str) -> tuple[int, int, int]:
    """Normalize two- or three-component Rust versions for comparison."""
    parts = version.split(".")
    if not 2 <= len(parts) <= 3 or any(not part.isdigit() for part in parts):
        raise ValueError(f"invalid Rust version: {version!r}")
    major, minor, *patch = (int(part) for part in parts)
    return major, minor, patch[0] if patch else 0


def brace_depth_at(text: str, stop: int) -> int:
    """Return Rust brace depth before ``stop``, ignoring comments and literals."""
    depth = 0
    index = 0
    block_comment_depth = 0
    while index < stop:
        if block_comment_depth:
            if text.startswith("/*", index):
                block_comment_depth += 1
                index += 2
            elif text.startswith("*/", index):
                block_comment_depth -= 1
                index += 2
            else:
                index += 1
            continue

        if text.startswith("//", index):
            newline = text.find("\n", index + 2, stop)
            index = stop if newline < 0 else newline + 1
            continue
        if text.startswith("/*", index):
            block_comment_depth = 1
            index += 2
            continue

        char = text[index]
        if char == '"':
            index += 1
            while index < stop:
                if text[index] == "\\":
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            continue
        if char == "'" and index + 2 < stop:
            closing = index + 2 if text[index + 1] != "\\" else index + 3
            if closing < stop and text[closing] == "'":
                index = closing + 1
                continue
        if char == "r":
            raw = re.match(r'r(#+)?"', text[index:stop])
            if raw:
                hashes = raw.group(1) or ""
                terminator = '"' + hashes
                end = text.find(terminator, index + raw.end(), stop)
                index = stop if end < 0 else end + len(terminator)
                continue

        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        index += 1
    return depth


def next_derived_item(text: str, start: int) -> str:
    """Return the first token after derive and any following attributes."""
    tail = text[start:]
    while True:
        tail = tail.lstrip()
        attribute = re.match(r"#\s*\[[^]]*\]", tail)
        if attribute is None:
            break
        tail = tail[attribute.end() :]
    match = re.match(r"(?:pub(?:\([^)]*\))?\s+)?([A-Za-z_][A-Za-z0-9_]*)", tail)
    return match.group(1) if match else ""


def module_candidates(parent: Path, name: str) -> tuple[Path, Path]:
    """Return the two conventional Rust source paths for a module declaration."""
    if parent.name in {"lib.rs", "main.rs", "mod.rs"}:
        module_root = parent.parent
    else:
        module_root = parent.parent / parent.stem
    return module_root / f"{name}.rs", module_root / name / "mod.rs"


class SourceValidator:
    """Validate Rust reachability, shader connectivity, and source-tree hygiene."""

    def __init__(self) -> None:
        self.errors: list[str] = []
        self.connected: set[Path] = set()

    def validate(self) -> list[str]:
        self._validate_rust_version()
        self._validate_workspace_boundaries()
        self._validate_rust_modules()
        self._validate_shaders()
        self._validate_generated_binaries()
        return self.errors

    def _read_text(self, path: Path, purpose: str) -> str | None:
        try:
            return path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            self.errors.append(f"cannot read {purpose} {relative_display(path)}: {error}")
            return None

    def _validate_rust_version(self) -> None:
        try:
            with (ROOT / "Cargo.toml").open("rb") as handle:
                manifest = tomllib.load(handle)
            with (ROOT / "rust-toolchain.toml").open("rb") as handle:
                toolchain = tomllib.load(handle)
            manifest_rust = str(manifest["workspace"]["package"]["rust-version"])
            pinned_rust = str(toolchain["toolchain"]["channel"])
            if normalized_rust_version(manifest_rust) != normalized_rust_version(pinned_rust):
                self.errors.append(
                    "Rust version mismatch: Cargo.toml declares "
                    f"{manifest_rust}, rust-toolchain.toml pins {pinned_rust}"
                )
        except (KeyError, OSError, ValueError, tomllib.TOMLDecodeError) as error:
            self.errors.append(f"cannot validate the pinned Rust version: {error}")

    def _validate_workspace_boundaries(self) -> None:
        expected_members = {
            "crates/auraw-core",
            "crates/auraw-gpu",
            "crates/auraw-ai",
            "crates/auraw-ui",
            "crates/auraw-ffi",
            "crates/auraw-cli",
            "xtask",
        }
        restricted = {
            "ort": "auraw-ai",
            "eframe": "auraw-ui",
            "jni": "auraw-ffi",
        }
        try:
            with (ROOT / "Cargo.toml").open("rb") as handle:
                root_manifest = tomllib.load(handle)
            members = set(root_manifest["workspace"]["members"])
        except (KeyError, OSError, tomllib.TOMLDecodeError) as error:
            self.errors.append(f"cannot validate workspace members: {error}")
            return

        if members != expected_members:
            self.errors.append(
                "workspace members differ from the required six production crates plus xtask: "
                f"{sorted(members)}"
            )

        for member in sorted(expected_members):
            manifest_path = ROOT / member / "Cargo.toml"
            try:
                with manifest_path.open("rb") as handle:
                    manifest = tomllib.load(handle)
            except (OSError, tomllib.TOMLDecodeError) as error:
                self.errors.append(
                    f"cannot read workspace manifest {relative_display(manifest_path)}: {error}"
                )
                continue

            package_name = manifest.get("package", {}).get("name")
            dependency_tables = [
                value
                for key, value in manifest.items()
                if key in {"dependencies", "dev-dependencies", "build-dependencies"}
                and isinstance(value, dict)
            ]
            for target in manifest.get("target", {}).values():
                if not isinstance(target, dict):
                    continue
                dependency_tables.extend(
                    value
                    for key, value in target.items()
                    if key in {"dependencies", "dev-dependencies", "build-dependencies"}
                    and isinstance(value, dict)
                )

            declared = set().union(*(table.keys() for table in dependency_tables))
            for dependency, owner in restricted.items():
                if dependency in declared and package_name != owner:
                    self.errors.append(
                        f"{dependency} must only be declared by {owner}, not {package_name}"
                    )

            source_root = manifest_path.parent / "src"
            for source in source_root.rglob("*.rs"):
                text = self._read_text(source, "Rust source")
                if text is None:
                    continue
                for dependency, owner in restricted.items():
                    if package_name == owner:
                        continue
                    if re.search(rf"(?<![A-Za-z0-9_]){re.escape(dependency)}::", text):
                        self.errors.append(
                            f"{dependency} API used outside {owner}: {relative_display(source)}"
                        )

    def _validate_rust_modules(self) -> None:
        crate_sources = sorted((ROOT / "crates").glob("*/src"))
        if not crate_sources:
            self.errors.append("missing workspace crate sources under crates/*/src")
            return

        roots: list[Path] = []
        for source in crate_sources:
            roots.extend(path for path in (source / "lib.rs", source / "main.rs") if path.is_file())
            roots.extend(sorted((source / "bin").glob("*.rs")))
        if not roots:
            self.errors.append("no Rust crate roots found in the workspace")
            return

        for root in roots:
            self._visit_module(root)

        for source in crate_sources:
            for module in sorted(source.rglob("*.rs")):
                if module.resolve() not in self.connected:
                    self.errors.append(
                        "stale Rust source is not reachable from a crate root: "
                        f"{relative_display(module)}"
                    )

    def _visit_module(self, module: Path) -> None:
        module = module.resolve()
        if module in self.connected:
            return
        if not module.is_file():
            self.errors.append(f"referenced Rust source is missing: {relative_display(module)}")
            return

        self.connected.add(module)
        text = self._read_text(module, "Rust source")
        if text is None:
            return

        for include_match in INCLUDE_RE.finditer(text):
            if brace_depth_at(text, include_match.start()) != 0:
                self.errors.append(
                    f"include! must appear at module scope in {relative_display(module)}: "
                    f"{include_match.group(1)}"
                )

        for derive_match in DERIVE_RE.finditer(text):
            item = next_derived_item(text, derive_match.end())
            if item not in {"struct", "enum", "union"}:
                self.errors.append(
                    f"derive attribute in {relative_display(module)} is attached to "
                    f"{item or 'no item'}"
                )

        for include_name in INCLUDE_RE.findall(text):
            included = (module.parent / include_name).resolve()
            if included.is_file():
                self._visit_module(included)
            else:
                self.errors.append(
                    f"include! in {relative_display(module)} references missing file: "
                    f"{include_name}"
                )

        path_modules = {name for _, name in PATH_MODULE_RE.findall(text)}
        for relative, name in PATH_MODULE_RE.findall(text):
            target = (module.parent / relative).resolve()
            if target.is_file():
                self._visit_module(target)
            else:
                self.errors.append(
                    f"module {name!r} declared by {relative_display(module)} "
                    f"references missing source: {relative}"
                )

        for name in MODULE_RE.findall(text):
            if name in path_modules:
                continue
            direct, nested = module_candidates(module, name)
            matches = [candidate for candidate in (direct, nested) if candidate.is_file()]
            if len(matches) == 1:
                self._visit_module(matches[0])
            elif not matches:
                self.errors.append(
                    f"module {name!r} declared by {relative_display(module)} has no source file"
                )
            else:
                self.errors.append(
                    f"module {name!r} declared by {relative_display(module)} is ambiguous: "
                    f"{relative_display(direct)} and {relative_display(nested)}"
                )

    def _validate_shaders(self) -> None:
        gpu_root = ROOT / "crates/auraw-gpu"
        shader_dir = gpu_root / "src/shaders"
        if not shader_dir.is_dir():
            self.errors.append("missing shader source directory: crates/auraw-gpu/src/shaders")
            return

        shader_names = {path.name for path in shader_dir.glob("*.wgsl")}
        build_rs = self._read_text(gpu_root / "build.rs", "GPU build script")
        if build_rs is not None:
            watched = set(re.findall(r'"([^"\\]+\.wgsl)"', build_rs))
            for name in sorted(shader_names - watched):
                self.errors.append(f"WGSL file is not watched by auraw-gpu/build.rs: {name}")
            for name in sorted(watched - shader_names):
                self.errors.append(f"auraw-gpu/build.rs watches a missing WGSL file: {name}")

        rust_sources: list[str] = []
        for path in (gpu_root / "src").rglob("*.rs"):
            source = self._read_text(path, "Rust source")
            if source is not None:
                rust_sources.append(source)
        included = set(
            re.findall(
                r'include_str!\("\.\./shaders/([^"\\]+\.wgsl)"\)',
                "\n".join(rust_sources),
            )
        )

        preprocessor_path = gpu_root / "build_support/shader_preprocessor.rs"
        preprocessor = self._read_text(preprocessor_path, "shader preprocessor")
        if preprocessor is not None:
            generated_templates = set(
                re.findall(
                    r'\("([^"\\]+\.wgsl)",\s*"[^"\\]+\.generated\.wgsl"\)',
                    preprocessor,
                )
            )
            for template_name in generated_templates:
                template = shader_dir / template_name
                if not template.is_file():
                    continue
                included.add(template_name)
                template_source = self._read_text(template, "shader template")
                if template_source is None:
                    continue
                included.update(
                    re.findall(r'//\s*@include\s+"([^"\\]+\.wgsl)"', template_source)
                )

        for name in sorted(shader_names - included):
            self.errors.append(f"WGSL file is not included by auraw-gpu Rust source: {name}")

    def _validate_generated_binaries(self) -> None:
        for path in sorted(item for item in ROOT.rglob("*") if item.is_file()):
            relative = path.relative_to(ROOT).as_posix()
            if any(
                relative == root or relative.startswith(f"{root}/")
                for root in IGNORED_BINARY_ROOTS
            ):
                continue
            if path.suffix.lower() in BINARY_SUFFIXES and relative not in ALLOWED_BINARY_PATHS:
                self.errors.append(f"generated binary is present in the source tree: {relative}")


def validate_source() -> list[str]:
    """Run all source-tree checks."""
    return SourceValidator().validate()


def validate_workflows() -> list[str]:
    """Reject mutable third-party action references in CI workflows."""
    errors: list[str] = []
    workflow_roots = (ROOT / ".github/workflows", ROOT / ".gitea/workflows")
    for workflow_root in workflow_roots:
        if not workflow_root.is_dir():
            continue
        paths = sorted((*workflow_root.rglob("*.yml"), *workflow_root.rglob("*.yaml")))
        for path in paths:
            try:
                lines = path.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as error:
                errors.append(f"cannot read workflow {relative_display(path)}: {error}")
                continue
            for line_number, line in enumerate(lines, 1):
                match = WORKFLOW_USES_RE.search(line)
                if match is None:
                    continue
                action = match.group(1).strip("'\"")
                if action.startswith(("./", "docker://")):
                    continue
                revision = action.rsplit("@", 1)[1] if "@" in action else ""
                if COMMIT_SHA_RE.fullmatch(revision) is None:
                    errors.append(
                        f"{relative_display(path)}:{line_number}: "
                        f"mutable action reference {action}"
                    )
    return errors


def validate_gradle() -> list[str]:
    """Verify the checked-in Gradle wrapper before it is executed."""
    errors: list[str] = []
    required_files = (GRADLE_PROPERTIES, GRADLE_WRAPPER_JAR, GRADLEW, GRADLEW_BAT)
    for path in required_files:
        if not path.is_file():
            errors.append(f"missing wrapper file: {relative_display(path)}")
    if errors:
        return errors

    try:
        properties = parse_properties(GRADLE_PROPERTIES)
    except (OSError, UnicodeError, ValueError) as error:
        return [str(error)]

    distribution_url = properties.get("distributionUrl", "").replace("\\:", ":")
    expected_suffix = f"/gradle-{EXPECTED_GRADLE_VERSION}-bin.zip"
    if not distribution_url.startswith("https://services.gradle.org/distributions/"):
        errors.append("distributionUrl must use the official HTTPS Gradle distribution host")
    if not distribution_url.endswith(expected_suffix):
        errors.append(
            f"distributionUrl must select Gradle {EXPECTED_GRADLE_VERSION}; "
            f"found {distribution_url or '<missing>'}"
        )
    if properties.get("distributionSha256Sum") != EXPECTED_GRADLE_DISTRIBUTION_SHA256:
        errors.append("distributionSha256Sum does not match the pinned Gradle distribution")
    if properties.get("validateDistributionUrl", "").lower() != "true":
        errors.append("validateDistributionUrl must remain enabled")

    try:
        actual_jar_sha256 = sha256(GRADLE_WRAPPER_JAR)
    except OSError as error:
        errors.append(f"cannot hash {relative_display(GRADLE_WRAPPER_JAR)}: {error}")
    else:
        if actual_jar_sha256 != EXPECTED_GRADLE_WRAPPER_JAR_SHA256:
            errors.append(
                "gradle-wrapper.jar checksum mismatch: expected "
                f"{EXPECTED_GRADLE_WRAPPER_JAR_SHA256}, found {actual_jar_sha256}"
            )

    try:
        if not GRADLEW.stat().st_mode & stat.S_IXUSR:
            errors.append("gradlew must be executable")
        shell_script = GRADLEW.read_text(encoding="utf-8")
        batch_script = GRADLEW_BAT.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        errors.append(f"cannot inspect Gradle launcher scripts: {error}")
        return errors

    wrapper_path = "gradle/wrapper/gradle-wrapper.jar"
    if wrapper_path not in shell_script.replace("$APP_HOME/", ""):
        errors.append("gradlew does not reference the checked-in wrapper JAR")
    if not re.search(r"gradle[\\/]wrapper[\\/]gradle-wrapper\.jar", batch_script, re.I):
        errors.append("gradlew.bat does not reference the checked-in wrapper JAR")
    return errors


CHECK_SOURCE = CheckSpec(
    title="Source tree",
    success_message="connected Rust modules, tracked shaders, and source-tree binaries verified",
    validate=validate_source,
)
CHECK_WORKFLOWS = CheckSpec(
    title="Workflow pins",
    success_message="all third-party workflow actions are pinned to full commit SHAs",
    validate=validate_workflows,
)
CHECK_GRADLE = CheckSpec(
    title="Gradle wrapper",
    success_message=(
        f"Gradle wrapper {EXPECTED_GRADLE_VERSION} integrity verified "
        f"({EXPECTED_GRADLE_WRAPPER_JAR_SHA256})"
    ),
    validate=validate_gradle,
)
ALL_CHECKS = (CHECK_SOURCE, CHECK_WORKFLOWS, CHECK_GRADLE)


def run_checks(checks: Sequence[CheckSpec]) -> int:
    """Run checks, print consistent output, and return a CI-friendly status."""
    failed = 0
    for check in checks:
        print(f"== {check.title} ==", flush=True)
        try:
            errors = check.validate()
        except Exception as error:  # Keep CI output actionable for unexpected failures.
            errors = [f"unexpected {type(error).__name__}: {error}"]

        if errors:
            failed += 1
            print(f"FAIL: {len(errors)} issue(s)", file=sys.stderr, flush=True)
            for error in errors:
                print(f"  - {error}", file=sys.stderr)
        else:
            print(f"PASS: {check.success_message}")
        print()

    if len(checks) > 1:
        passed = len(checks) - failed
        print(f"Validation summary: {passed} passed, {failed} failed")
    return 1 if failed else 0


def run_cargo_test(test_filter: str, *, release: bool) -> bool:
    """Run one compiler-backed analytical Rust test filter."""
    command = ["cargo", "test", "--locked", "--lib"]
    if release:
        command.append("--release")
    command += [test_filter, "--", "--nocapture"]
    print(f"  $ {' '.join(command)}", flush=True)
    try:
        completed = subprocess.run(command, cwd=ROOT, check=False)
    except OSError as error:
        print(f"  unable to execute cargo: {error}", file=sys.stderr)
        return False
    return completed.returncode == 0


def command_validate_math(args: argparse.Namespace) -> int:
    """Run static and analytical camera-profile and demosaic validation."""
    if shutil.which("cargo") is None:
        print(
            "error: cargo is required because math validation compiles Rust and "
            "validates WGSL with Naga",
            file=sys.stderr,
        )
        return 2

    failed: list[str] = []
    total = sum(len(filters) for _, filters in MATH_TEST_GROUPS)
    for group_name, filters in MATH_TEST_GROUPS:
        print(f"== {group_name.title()} validation ({len(filters)} test filters) ==")
        for test_filter in filters:
            if not run_cargo_test(test_filter, release=args.release):
                failed.append(test_filter)
        print()

    if failed:
        print(
            f"Math validation failed for {len(failed)} of {total} test filters:",
            file=sys.stderr,
        )
        for test_filter in failed:
            print(f"  - {test_filter}", file=sys.stderr)
        return 1

    mode = "release" if args.release else "debug"
    print(f"PASS: all {total} compiler-backed math test filters passed ({mode} mode)")
    return 0

SCENE_MIDDLE_GREY = 0.1845
LUMA = (0.2627002, 0.6779981, 0.0593017)
SCENE_INPUTS = [
    0.0,
    1e-12,
    1e-10,
    1e-8,
    5e-8,
    1e-7,
    5e-7,
    1e-6,
    1e-5,
    3e-5,
    1e-4,
    3e-4,
    1e-3,
    3e-3,
    1e-2,
    3e-2,
    0.1,
    0.18,
    0.5,
    1.0,
    4.0,
]
DISPLAY_INPUTS = [
    0.0,
    1e-12,
    1e-10,
    1e-8,
    5e-8,
    1e-7,
    5e-7,
    1e-6,
    1e-5,
    1e-4,
    1e-3,
    1e-2,
    0.05,
    0.10,
    0.149999,
    0.15,
    0.150001,
    0.20,
    0.50,
    1.0,
]
SETTINGS = [-100, -75, -50, -25, 0, 25, 50, 75, 100]
DEFAULT_PERCENTILES = (-5.0, 0.0)
COLOR_RATIOS = {
    "neutral": (1.0, 1.0, 1.0),
    "red": (1.0, 0.0, 0.0),
    "orange": (1.0, 0.5, 0.0),
    "yellow": (1.0, 1.0, 0.0),
    "green": (0.0, 1.0, 0.0),
    "cyan": (0.0, 1.0, 1.0),
    "blue": (0.0, 0.0, 1.0),
    "magenta": (1.0, 0.0, 1.0),
}

# AuRaw/darktable-default sigmoid coefficients in src/pipeline/sigmoid.rs.
SIGMOID_WHITE = 1.0
SIGMOID_LOG2_PAPER_EXPOSURE = -1.4751521
SIGMOID_FILM_FOG = 0.0013843221
SIGMOID_FILM_POWER = 1.4909091
SIGMOID_PAPER_POWER = 1.0

Rgb = tuple[float, float, float]


def clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def dot(a: Rgb, b: Rgb) -> float:
    return sum(x * y for x, y in zip(a, b))


def scale(rgb: Rgb, factor: float) -> Rgb:
    return tuple(channel * factor for channel in rgb)  # type: ignore[return-value]


def rgb_for_luminance(ratio: Rgb, luminance: float) -> Rgb:
    basis_luminance = dot(ratio, LUMA)
    if luminance <= 0.0 or basis_luminance <= 0.0:
        return (0.0, 0.0, 0.0)
    return scale(ratio, luminance / basis_luminance)


def remap_luminance(rgb: Rgb, target_luminance: float) -> Rgb:
    source_luminance = dot(rgb, LUMA)
    if source_luminance <= 0.0:
        return rgb
    return scale(rgb, target_luminance / source_luminance)


def smoothstep(a: float, b: float, x: float) -> float:
    t = clamp((x - a) / max(b - a, 1e-6), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def shaped(v: float) -> float:
    n = clamp(v / 100.0, -1.0, 1.0)
    magnitude = abs(n)
    return math.copysign(magnitude * (1.45 - 0.45 * magnitude), n) if magnitude else 0.0


def shadow_range(p05: float, p50: float) -> tuple[float, float]:
    return p05 - 0.90, p50 + 1.35


def shadow_mask(ev: float, bounds: tuple[float, float]) -> float:
    lower, upper = bounds
    return 1.0 - smoothstep(lower, upper, ev)


def shadows_scene(y: float, setting: float, percentiles=DEFAULT_PERCENTILES) -> tuple[float, float, float]:
    if y <= 0.0:
        return y, 0.0, 0.0
    ev = math.log2(y / SCENE_MIDDLE_GREY)
    bounds = shadow_range(*percentiles)
    weight = shadow_mask(ev, bounds)
    amount = shaped(setting)
    lower, upper = bounds
    limit = 0.64 * max(upper - lower, 0.25)
    strength = math.copysign(min(abs(amount) * 2.20, limit), amount) if amount else 0.0
    delta_ev = strength * weight
    return y * 2.0**delta_ev, delta_ev, weight


def sigmoid(y: float) -> float:
    base = SIGMOID_FILM_FOG + max(y, 0.0)
    if base <= 0.0:
        return 0.0
    log2_film = SIGMOID_FILM_POWER * math.log2(base)
    log_ratio = log2_film - SIGMOID_LOG2_PAPER_EXPOSURE
    if log_ratio >= 0.0:
        ratio = 1.0 / (1.0 + 2.0 ** (-log_ratio))
    else:
        z = 2.0**log_ratio
        ratio = z / (1.0 + z)
    return SIGMOID_WHITE * clamp(ratio, 0.0, 1.0) ** SIGMOID_PAPER_POWER


def blacks_display(y: float, setting: float) -> tuple[float, float, float]:
    if y <= 0.0 or setting == 0.0:
        return y, 0.0, 0.0
    amount = shaped(setting)
    hdr_guard = 1.0 - smoothstep(0.35, 1.0, y)
    if amount >= 0.0:
        weight = (0.08 + 0.92 * 2.0 ** (-y / 0.035)) * hdr_guard
        delta_ev = amount * 1.75 * weight
    else:
        deep = 1.0 - smoothstep(0.012, 0.030, y)
        tail = 0.10 + 2.35 * 2.0 ** (-y / 0.070)
        weight = (10.50 * deep + tail) * hdr_guard
        delta_ev = -(-amount) * weight
    return y * 2.0**delta_ev, delta_ev, weight


def signed_cuberoot(value: float) -> float:
    return math.copysign(abs(value) ** (1.0 / 3.0), value)


def rec2020_to_oklab(rgb: Rgb) -> Rgb:
    r, g, b = rgb
    x = 0.6369580 * r + 0.1446169 * g + 0.1688809 * b
    y = 0.2627002 * r + 0.6779981 * g + 0.0593017 * b
    z = 0.0000000 * r + 0.0280727 * g + 1.0609851 * b
    sr = 3.24096994 * x - 1.53738318 * y - 0.49861076 * z
    sg = -0.96924364 * x + 1.87596750 * y + 0.04155506 * z
    sb = 0.05563008 * x - 0.20397696 * y + 1.05697151 * z
    l = signed_cuberoot(0.4122214708 * sr + 0.5363325363 * sg + 0.0514459929 * sb)
    m = signed_cuberoot(0.2119034982 * sr + 0.6806995451 * sg + 0.1073969566 * sb)
    s = signed_cuberoot(0.0883024619 * sr + 0.2817188376 * sg + 0.6299787005 * sb)
    return (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )


def color_metrics(input_rgb: Rgb, output_rgb: Rgb) -> dict[str, float]:
    input_luma = dot(input_rgb, LUMA)
    output_luma = dot(output_rgb, LUMA)
    if input_luma > 0.0 and output_luma > 0.0:
        ratio_residual = max(
            abs(output_rgb[index] / output_luma - input_rgb[index] / input_luma)
            for index in range(3)
        )
    else:
        ratio_residual = 0.0

    input_lab = rec2020_to_oklab(input_rgb)
    output_lab = rec2020_to_oklab(output_rgb)
    input_chroma = math.hypot(input_lab[1], input_lab[2])
    output_chroma = math.hypot(output_lab[1], output_lab[2])
    input_hue = math.degrees(math.atan2(input_lab[2], input_lab[1])) if input_chroma > 1e-12 else 0.0
    output_hue = math.degrees(math.atan2(output_lab[2], output_lab[1])) if output_chroma > 1e-12 else 0.0
    hue_shift = (output_hue - input_hue + 180.0) % 360.0 - 180.0
    input_normalized_chroma = input_chroma / max(abs(input_lab[0]), 1e-12)
    output_normalized_chroma = output_chroma / max(abs(output_lab[0]), 1e-12)
    return {
        "rgb_ratio_residual": ratio_residual,
        "oklab_hue_input_degrees": input_hue,
        "oklab_hue_output_degrees": output_hue,
        "oklab_hue_shift_degrees": hue_shift,
        "normalized_chroma_input": input_normalized_chroma,
        "normalized_chroma_output": output_normalized_chroma,
        "normalized_chroma_change": output_normalized_chroma - input_normalized_chroma,
    }


def row(
    *,
    control: str,
    setting: float,
    color: str,
    domain: str,
    input_luminance: float,
    input_rgb: Rgb,
    operation_rgb: Rgb,
    display_rgb: Rgb,
    delta_ev: float,
    weight: float,
) -> dict[str, object]:
    metrics = color_metrics(input_rgb, operation_rgb)
    return {
        "control": control,
        "setting": setting,
        "color": color,
        "operation_domain": domain,
        "input_luminance": input_luminance,
        "input_r": input_rgb[0],
        "input_g": input_rgb[1],
        "input_b": input_rgb[2],
        "operation_output_luminance": dot(operation_rgb, LUMA),
        "operation_output_r": operation_rgb[0],
        "operation_output_g": operation_rgb[1],
        "operation_output_b": operation_rgb[2],
        "effective_ev_change": delta_ev,
        "effective_mask_weight": weight,
        "display_output_luminance": dot(display_rgb, LUMA),
        "display_output_r": display_rgb[0],
        "display_output_g": display_rgb[1],
        "display_output_b": display_rgb[2],
        **metrics,
    }


def rows() -> Iterable[dict[str, object]]:
    for setting in SETTINGS:
        for luminance in SCENE_INPUTS:
            scene_out, delta_ev, weight = shadows_scene(luminance, setting)
            for color, ratio in COLOR_RATIOS.items():
                input_rgb = rgb_for_luminance(ratio, luminance)
                operation_rgb = remap_luminance(input_rgb, scene_out)
                display_rgb = remap_luminance(operation_rgb, sigmoid(scene_out))
                yield row(
                    control="Shadows",
                    setting=setting,
                    color=color,
                    domain="scene-linear",
                    input_luminance=luminance,
                    input_rgb=input_rgb,
                    operation_rgb=operation_rgb,
                    display_rgb=display_rgb,
                    delta_ev=delta_ev,
                    weight=weight,
                )

    for setting in SETTINGS:
        for luminance in DISPLAY_INPUTS:
            display_out, delta_ev, weight = blacks_display(luminance, setting)
            for color, ratio in COLOR_RATIOS.items():
                input_rgb = rgb_for_luminance(ratio, luminance)
                operation_rgb = remap_luminance(input_rgb, display_out)
                yield row(
                    control="Blacks",
                    setting=setting,
                    color=color,
                    domain="display-linear",
                    input_luminance=luminance,
                    input_rgb=input_rgb,
                    operation_rgb=operation_rgb,
                    display_rgb=operation_rgb,
                    delta_ev=delta_ev,
                    weight=weight,
                )

def command_analyze_low_tone(args: argparse.Namespace) -> int:
    """Write the analytical Shadows/Blacks response table."""
    data = list(rows())
    fields = list(data[0])
    if args.csv:
        args.csv.parent.mkdir(parents=True, exist_ok=True)
        stream = args.csv.open("w", newline="", encoding="utf-8")
    else:
        stream = sys.stdout
    try:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(data)
    finally:
        if args.csv:
            stream.close()
    return 0

BENCHMARK_SCENES = {
    "synthetic-bayer-multitarget": ("synthetic-bayer.dng", 256, 256),
    "synthetic-xtrans-multitarget": ("synthetic-xtrans.dng", 256, 256),
}


def percentile_95(values: list[float]) -> float:
    ordered = sorted(values)
    return ordered[max(0, int(len(ordered) * 0.95) - 1)]


def render_command(renderer: Path, source: Path, target: Path) -> list[str]:
    return [
        str(renderer),
        "--backend",
        "gpu",
        "--input",
        str(source),
        "--output",
        str(target),
    ]

def command_bench(args: argparse.Namespace) -> int:
    """Run or dry-run the canonical GPU renderer benchmark."""
    if args.runs < 1:
        print("error: --runs must be positive", file=sys.stderr)
        return 2

    renderer = args.renderer if args.renderer.is_absolute() else ROOT / args.renderer
    output = args.output if args.output.is_absolute() else ROOT / args.output
    budget_file = args.budget_file if args.budget_file.is_absolute() else ROOT / args.budget_file

    scene_inputs: dict[str, tuple[Path, int, int]] = {}
    for scene, (filename, width, height) in BENCHMARK_SCENES.items():
        source = ROOT / "regression/raw" / filename
        if not source.is_file():
            print(f"error: committed benchmark scene is missing: {source}", file=sys.stderr)
            return 2
        scene_inputs[scene] = (source, width, height)

    if args.dry_run:
        for scene, (source, _, _) in scene_inputs.items():
            target = ROOT / "target/benchmarks" / f"{scene}-1.npz"
            print(shlex.join(render_command(renderer, source, target)))
        return 0

    if not renderer.is_file():
        print(f"error: renderer does not exist: {renderer}", file=sys.stderr)
        return 2

    measured: dict[str, list[float]] = {scene: [] for scene in BENCHMARK_SCENES}
    warmups: dict[str, float] = {}
    for scene, (source, _, _) in scene_inputs.items():
        for run in range(args.runs + 1):
            target = ROOT / "target/benchmarks" / f"{scene}-{run}.npz"
            target.parent.mkdir(parents=True, exist_ok=True)
            started = time.perf_counter()
            subprocess.run(render_command(renderer, source, target), check=True)
            elapsed_ms = (time.perf_counter() - started) * 1000.0
            if run == 0:
                warmups[scene] = elapsed_ms
            else:
                measured[scene].append(elapsed_ms)

    scene_reports: dict[str, dict[str, object]] = {}
    for scene, times in measured.items():
        _, width, height = scene_inputs[scene]
        megapixels = width * height / 1_000_000.0
        median_ms = statistics.median(times)
        scene_reports[scene] = {
            "width": width,
            "height": height,
            "megapixels": megapixels,
            "warmup_ms": warmups[scene],
            "times_ms": times,
            "median_ms": median_ms,
            "p95_ms": percentile_95(times),
            "median_megapixels_per_second": megapixels / (median_ms / 1000.0),
        }

    budget = json.loads(budget_file.read_text(encoding="utf-8"))
    minimum_throughput = float(budget["budgets"]["export_mp_per_second_min"])
    maximum_startup = float(budget["budgets"]["startup_shader_compile_p95_ms"])
    throughput_pass = all(
        float(scene["median_megapixels_per_second"]) >= minimum_throughput
        for scene in scene_reports.values()
    )
    startup_pass = max(warmups.values()) <= maximum_startup
    budget_result = {
        "budget_file": str(budget_file.relative_to(ROOT)),
        "export_throughput_pass": throughput_pass,
        "startup_pass": startup_pass,
        "passed": throughput_pass and startup_pass,
    }
    report = {
        "schema": 2,
        "renderer": str(renderer),
        "runs": args.runs,
        "scenes": scene_reports,
        "budget": budget_result,
        "measurement_scope": (
            "wall-clock process startup plus canonical GPU render/readback; "
            "use native GPU timestamp queries for per-pass diagnosis"
        ),
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(output)
    return 1 if args.enforce_budget and not budget_result["passed"] else 0

if np is not None:
    SRGB_LUMA = np.array([0.2126729, 0.7151522, 0.0721750], dtype=np.float32)
    ADOBE_RGB_LUMA = np.array([0.29734498, 0.62736357, 0.07529146], dtype=np.float32)
else:
    SRGB_LUMA = (0.2126729, 0.7151522, 0.0721750)
    ADOBE_RGB_LUMA = (0.29734498, 0.62736357, 0.07529146)
ADOBE_RGB_GAMMA = 2.0 + 51.0 / 256.0
LUMA_QUANTILES = (
    np.array(
        [0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50,
         0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0],
        dtype=np.float64,
    )
    if np is not None
    else (0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50,
          0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0)
)


@dataclass(frozen=True)
class Endpoint:
    name: str
    auraw_file: str
    lightroom_file: str
    detail_control: bool = False


ENDPOINTS = (
    Endpoint("Exposure +1.25", "exposure_plus1_25.png", "Exposure +1.25.tif"),
    Endpoint("Exposure -1.25", "exposure_minus1_25.png", "Exposure -1.25.tif"),
    Endpoint("Exposure +5", "exposure_plus5.png", "Exposure +5.tif"),
    Endpoint("Exposure -5", "exposure_minus5.png", "Exposure -5.tif"),
    Endpoint("Contrast +100", "contrast_plus100.png", "Contrast +100.tif"),
    Endpoint("Contrast -100", "contrast_minus100.png", "Contrast -100.tif"),
    Endpoint("Highlights +100", "highlights_plus100.png", "Highlights +100.tif"),
    Endpoint("Highlights -100", "highlights_minus100.png", "Highlights -100.tif"),
    Endpoint("Shadows +100", "shadows_plus100.png", "Shadows +100.tif"),
    Endpoint("Shadows -100", "shadows_minus100.png", "Shadows -100.tif"),
    Endpoint("Whites +100", "whites_plus100.png", "Whites +100.tif"),
    Endpoint("Whites -100", "whites_minus100.png", "Whites -100.tif"),
    Endpoint("Blacks +100", "blacks_plus100.png", "Blacks +100.tif"),
    Endpoint("Blacks -100", "blacks_minus100.png", "Blacks -100.tif"),
    Endpoint("Texture +100", "texture_plus100.png", "Texture +100.tif", True),
    Endpoint("Texture -100", "texture_minus100.png", "Texture -100.tif", True),
    Endpoint("Clarity +100", "clarity_plus100.png", "Clarity +100.tif", True),
    Endpoint("Clarity -100", "clarity_minus100.png", "Clarity -100.tif", True),
    Endpoint("Dehaze +100", "dehaze_plus100.png", "Dehaze +100.tif"),
    Endpoint("Dehaze -100", "dehaze_minus100.png", "Dehaze -100.tif"),
    Endpoint("Vibrance +100", "vibrance_plus100.png", "Vibrance +100.tif"),
    # The supplied Lightroom filename contains this typo; the endpoint is Vibrance.
    Endpoint("Vibrance -100", "vibrance_minus100.png", "Vibration -100.tif"),
    Endpoint("Saturation +100", "saturation_plus100.png", "Saturation +100.tif"),
    Endpoint("Saturation -100", "saturation_minus100.png", "Saturation -100.tif"),
)


def parse_crop(value: str) -> tuple[int, int, int, int]:
    try:
        crop = tuple(int(part) for part in value.split(","))
    except ValueError as error:
        raise argparse.ArgumentTypeError("crop must contain integers: X,Y,WIDTH,HEIGHT") from error
    if len(crop) != 4 or min(crop) < 0 or crop[2] < 1 or crop[3] < 1:
        raise argparse.ArgumentTypeError("crop must be X,Y,WIDTH,HEIGHT with a positive size")
    return crop  # type: ignore[return-value]


def image_region_size(
    path: Path, crop: tuple[int, int, int, int] | None
) -> tuple[int, int]:
    from PIL import Image

    with Image.open(path) as image:
        source_width, source_height = image.size
    if crop is None:
        return source_width, source_height
    x, y, width, height = crop
    if x + width > source_width or y + height > source_height:
        raise ValueError(
            f"crop {crop} is outside {path.name} ({source_width}x{source_height})"
        )
    return width, height


def encoded_rgb16(
    path: Path,
    *,
    crop: tuple[int, int, int, int] | None,
    sample_step: int,
) -> np.ndarray:
    width, height = image_region_size(path, crop)
    sampled_width = (width + sample_step - 1) // sample_step
    sampled_height = (height + sample_step - 1) // sample_step
    command = ["magick", str(path)]
    if crop is not None:
        x, y, crop_width, crop_height = crop
        command += ["-crop", f"{crop_width}x{crop_height}+{x}+{y}", "+repage"]
    if sample_step > 1:
        # Point sampling avoids inventing spatial detail while reducing memory.
        command += ["-filter", "point", "-sample", f"{sampled_width}x{sampled_height}!"]
    command += ["-alpha", "off", "-depth", "16", "-endian", "LSB", "rgb:-"]
    try:
        result = subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError as error:
        message = error.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"ImageMagick could not decode {path}: {message}") from error
    expected_values = sampled_width * sampled_height * 3
    encoded = np.frombuffer(result.stdout, dtype="<u2")
    if encoded.size != expected_values:
        raise RuntimeError(
            f"ImageMagick returned {encoded.size} values for {path}; expected {expected_values}"
        )
    return encoded.astype(np.float32).reshape(sampled_height, sampled_width, 3) / 65535.0


def linear_rgb(
    path: Path,
    *,
    crop: tuple[int, int, int, int] | None,
    sample_step: int,
    color_space: str,
) -> np.ndarray:
    encoded = encoded_rgb16(path, crop=crop, sample_step=sample_step)
    if color_space == "adobe-rgb":
        return np.power(np.maximum(encoded, 0.0), ADOBE_RGB_GAMMA)
    return np.where(
        encoded <= 0.04045,
        encoded / 12.92,
        ((encoded + 0.055) / 1.055) ** 2.4,
    )


def luma_delta_ev(base: np.ndarray, edit: np.ndarray, weights: np.ndarray) -> np.ndarray:
    base_luma = base @ weights
    edit_luma = edit @ weights
    return np.log2(np.maximum(edit_luma, 1e-7) / np.maximum(base_luma, 1e-7))


def baseline_luma_quantiles(rgb: np.ndarray, weights: np.ndarray) -> np.ndarray:
    return np.quantile(rgb @ weights, [0.05, 0.50, 0.95])


def quantile_curve(base: np.ndarray, delta: np.ndarray, weights: np.ndarray) -> np.ndarray:
    # Rank bins stay populated even when a clipped endpoint contains many equal
    # black/white samples; threshold masks can otherwise create empty bins.
    order = np.argsort((base @ weights).reshape(-1), kind="stable")
    ranked_delta = delta.reshape(-1)[order]
    count = ranked_delta.size
    boundaries = np.rint(LUMA_QUANTILES * count).astype(np.int64)
    boundaries[0] = 0
    boundaries[-1] = count
    response = []
    for lower, upper in zip(boundaries[:-1], boundaries[1:]):
        upper = max(upper, lower + 1)
        response.append(float(np.median(ranked_delta[lower:min(upper, count)])))
    return np.asarray(response, dtype=np.float32)


def chroma_response(base: np.ndarray, edit: np.ndarray) -> float:
    base_max = np.max(base, axis=2)
    edit_max = np.max(edit, axis=2)
    base_chroma = (base_max - np.min(base, axis=2)) / np.maximum(base_max, 1e-5)
    edit_chroma = (edit_max - np.min(edit, axis=2)) / np.maximum(edit_max, 1e-5)
    selected = (base_chroma > 0.03) & (base_max > 0.002) & (base_max < 0.98)
    if not np.any(selected):
        return 1.0
    return float(np.median(edit_chroma[selected] / np.maximum(base_chroma[selected], 1e-5)))


def box_blur(image: np.ndarray, radius: int = 2) -> np.ndarray:
    padded = np.pad(image, radius, mode="reflect")
    total = np.zeros_like(image, dtype=np.float64)
    diameter = 2 * radius + 1
    for y in range(diameter):
        for x in range(diameter):
            total += padded[y : y + image.shape[0], x : x + image.shape[1]]
    return (total / (diameter * diameter)).astype(np.float32)


def detail_response(base: np.ndarray, edit: np.ndarray, weights: np.ndarray) -> float:
    base_ev = np.log2(np.maximum(base @ weights, 1e-5))
    edit_ev = np.log2(np.maximum(edit @ weights, 1e-5))
    base_residual = base_ev - box_blur(base_ev)
    edit_residual = edit_ev - box_blur(edit_ev)
    denominator = float(np.quantile(np.abs(base_residual), 0.90))
    return float(np.quantile(np.abs(edit_residual), 0.90)) / max(denominator, 1e-6)

def command_compare_lightroom(args: argparse.Namespace) -> int:
    """Compare isolated AuRaw and Lightroom adjustment responses."""
    if np is None:
        print("error: numpy is required for compare-lightroom", file=sys.stderr)
        return 2
    if args.sample_step < 1:
        print("error: --sample-step must be positive", file=sys.stderr)
        return 2
    if shutil.which("magick") is None:
        print("error: ImageMagick 7 (`magick`) is required for native 16-bit RGB decoding", file=sys.stderr)
        return 2

    try:
        auraw_base = linear_rgb(
            args.auraw_dir / args.auraw_baseline,
            crop=args.auraw_crop,
            sample_step=args.sample_step,
            color_space="srgb",
        )
        lightroom_base = linear_rgb(
            args.lightroom_dir / args.lightroom_baseline,
            crop=args.lightroom_crop,
            sample_step=args.sample_step,
            color_space="adobe-rgb",
        )
    except (OSError, ValueError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    if auraw_base.shape != lightroom_base.shape:
        print(
            "error: baseline dimensions differ after crop/sample: "
            f"AuRaw {auraw_base.shape[:2]}, Lightroom {lightroom_base.shape[:2]}",
            file=sys.stderr,
        )
        return 2

    auraw_baseline_luma = baseline_luma_quantiles(auraw_base, SRGB_LUMA)
    lightroom_baseline_luma = baseline_luma_quantiles(lightroom_base, ADOBE_RGB_LUMA)
    print("baseline linear-luma quantiles       p05       p50       p95")
    print("AuRaw                              " + " ".join(f"{value:9.4f}" for value in auraw_baseline_luma))
    print("Lightroom                          " + " ".join(f"{value:9.4f}" for value in lightroom_baseline_luma))
    print()
    print(
        f"{'endpoint':<21} {'curve MAE':>9} {'Au chroma':>10} {'LR chroma':>10} "
        f"{'Au detail':>10} {'LR detail':>10}"
    )
    print("-" * 76)
    missing = 0
    for endpoint in ENDPOINTS:
        auraw_path = args.auraw_dir / endpoint.auraw_file
        lightroom_path = args.lightroom_dir / endpoint.lightroom_file
        if not auraw_path.is_file() or not lightroom_path.is_file():
            print(f"{endpoint.name:<21} {'missing':>9}")
            missing += 1
            continue
        auraw_edit = linear_rgb(
            auraw_path, crop=args.auraw_crop, sample_step=args.sample_step, color_space="srgb"
        )
        lightroom_edit = linear_rgb(
            lightroom_path,
            crop=args.lightroom_crop,
            sample_step=args.sample_step,
            color_space="adobe-rgb",
        )
        auraw_curve = quantile_curve(
            auraw_base, luma_delta_ev(auraw_base, auraw_edit, SRGB_LUMA), SRGB_LUMA
        )
        lightroom_curve = quantile_curve(
            lightroom_base,
            luma_delta_ev(lightroom_base, lightroom_edit, ADOBE_RGB_LUMA),
            ADOBE_RGB_LUMA,
        )
        curve_mae = float(np.mean(np.abs(auraw_curve - lightroom_curve)))
        if endpoint.detail_control:
            au_detail = f"{detail_response(auraw_base, auraw_edit, SRGB_LUMA):.3f}"
            lr_detail = f"{detail_response(lightroom_base, lightroom_edit, ADOBE_RGB_LUMA):.3f}"
        else:
            au_detail = lr_detail = "-"
        print(
            f"{endpoint.name:<21} {curve_mae:9.3f} "
            f"{chroma_response(auraw_base, auraw_edit):10.3f} "
            f"{chroma_response(lightroom_base, lightroom_edit):10.3f} "
            f"{au_detail:>10} {lr_detail:>10}"
        )
    return 1 if missing else 0

ICON_BACKGROUND = (17, 24, 39, 255)
ICON_FOREGROUND = (255, 255, 255, 255)
ICON_OUTER_A = [(54, 18), (84, 88), (69, 88), (62, 70), (46, 70), (39, 88), (24, 88)]
ICON_INNER_A = [(51, 57), (57, 57), (54, 44)]


def scale_icon_points(points: list[tuple[int, int]], scale: float) -> list[tuple[float, float]]:
    return [(x * scale, y * scale) for x, y in points]


def render_icon(edge: int) -> Image.Image:
    supersampling = 4
    render_edge = edge * supersampling
    scale = render_edge / 108
    from PIL import Image, ImageDraw

    image = Image.new("RGBA", (render_edge, render_edge), ICON_BACKGROUND)
    draw = ImageDraw.Draw(image)
    draw.polygon(scale_icon_points(ICON_OUTER_A, scale), fill=ICON_FOREGROUND)
    draw.polygon(scale_icon_points(ICON_INNER_A, scale), fill=ICON_BACKGROUND)
    return image.resize((edge, edge), Image.Resampling.LANCZOS)

def command_icons(_args: argparse.Namespace) -> int:
    """Generate PNG and ICO release assets from the shared AuRaw mark."""
    from PIL import Image

    output = ROOT / "packaging/icons"
    output.mkdir(parents=True, exist_ok=True)
    icon_1024 = render_icon(1024)
    icon_1024.save(output / "auraw-1024.png", optimize=True)
    render_icon(256).save(output / "auraw-256.png", optimize=True)
    icon_1024.save(
        output / "auraw.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    return 0

CORPUS_WIDTH = CORPUS_HEIGHT = 256
CORPUS_BLACK = 512
CORPUS_WHITE = 16383

if np is not None:
    CORPUS_BAYER = np.asarray([[0, 1], [1, 2]], dtype=np.uint8)
    CORPUS_XTRANS = np.asarray(
        [
            [1, 2, 1, 1, 0, 1],
            [0, 1, 0, 2, 1, 2],
            [1, 2, 1, 1, 0, 1],
            [1, 0, 1, 1, 2, 1],
            [2, 1, 2, 0, 1, 0],
            [1, 0, 1, 1, 2, 1],
        ],
        dtype=np.uint8,
    )
else:
    CORPUS_BAYER = None
    CORPUS_XTRANS = None


def build_scene() -> np.ndarray:
    yy, xx = np.indices((CORPUS_HEIGHT, CORPUS_WIDTH), dtype=np.float32)
    rgb = np.empty((CORPUS_HEIGHT, CORPUS_WIDTH, 3), dtype=np.float32)
    rgb[..., 0] = 0.07 + 0.20 * xx / (CORPUS_WIDTH - 1)
    rgb[..., 1] = 0.08 + 0.18 * yy / (CORPUS_HEIGHT - 1)
    rgb[..., 2] = 0.10 + 0.12 * (xx + yy) / (CORPUS_WIDTH + CORPUS_HEIGHT - 2)

    # Neutral slanted edge: edge spread and direction response.
    edge = xx[24:120, 24:112] > 64.0 + 0.37 * (yy[24:120, 24:112] - 72.0)
    neutral = np.where(edge, 0.68, 0.065).astype(np.float32)
    rgb[24:120, 24:112, :] = neutral[..., None]

    # CFA alias suite: four deterministic near-Nyquist targets covering the
    # common failure modes that motivate dual demosaic. Keeping them in one
    # compact block lets Bayer and X-Trans fixtures share identical scene data.
    # 1) woven fabric with chromatic diagonal modulation.
    fy = yy[24:72, 136:188]
    fx = xx[24:72, 136:188]
    weave = 0.32 + 0.16 * np.sign(np.sin(fx * np.pi / 2.0) * np.sin(fy * np.pi / 3.0))
    diagonal = 0.055 * np.sin((fx + 1.7 * fy) * np.pi / 2.5)
    rgb[24:72, 136:188, 0] = weave + diagonal
    rgb[24:72, 136:188, 1] = weave
    rgb[24:72, 136:188, 2] = weave - diagonal

    # 2) neutral radial zone plate: orientation-independent alias stress.
    fy = yy[24:72, 188:240] - 48.0
    fx = xx[24:72, 188:240] - 214.0
    radius2 = fx * fx + fy * fy
    zone = 0.34 + 0.20 * np.sin(0.095 * radius2)
    rgb[24:72, 188:240, :] = zone[..., None]

    # 3) fine diagonal foliage-like luminance with green-biased microcontrast.
    fy = yy[72:120, 136:188]
    fx = xx[72:120, 136:188]
    leaf = 0.30 + 0.11 * np.sin((1.8 * fx + fy) * np.pi / 2.2)
    leaf += 0.07 * np.sin((fx - 1.4 * fy) * np.pi / 3.1)
    rgb[72:120, 136:188, 0] = leaf * 0.82
    rgb[72:120, 136:188, 1] = leaf * 1.08
    rgb[72:120, 136:188, 2] = leaf * 0.76

    # 4) one/two-pixel chromatic stripe crossings: false-colour stress.
    fy = yy[72:120, 188:240]
    fx = xx[72:120, 188:240]
    carrier = np.sign(np.sin(fx * np.pi / 1.5))
    cross = np.sign(np.sin((fx + fy) * np.pi / 2.0))
    base = 0.34 + 0.08 * np.sign(np.sin(fy * np.pi / 2.5))
    rgb[72:120, 188:240, 0] = base + 0.09 * carrier
    rgb[72:120, 188:240, 1] = base + 0.03 * cross
    rgb[72:120, 188:240, 2] = base - 0.09 * carrier

    # Flat, underexposed, high-ISO-like patch with deterministic chroma noise.
    rng = np.random.default_rng(0xA0_52)
    shadow = np.full((88, 88, 3), 0.025, dtype=np.float32)
    common = rng.normal(0.0, 0.0045, (88, 88, 1)).astype(np.float32)
    chroma = rng.normal(0.0, 0.0020, shadow.shape).astype(np.float32)
    rgb[144:232, 24:112, :] = shadow + common + chroma

    # Clipped neutral and coloured highlights with smooth shoulders.
    hy = yy[136:240, 128:240]
    hx = xx[136:240, 128:240]
    highlight = np.full((104, 112, 3), 0.12, dtype=np.float32)
    spots = [
        (158.0, 166.0, np.array([1.35, 0.22, 0.08], dtype=np.float32)),
        (205.0, 165.0, np.array([0.10, 1.30, 0.25], dtype=np.float32)),
        (161.0, 213.0, np.array([0.12, 0.28, 1.40], dtype=np.float32)),
        (211.0, 212.0, np.array([1.35, 1.35, 1.35], dtype=np.float32)),
    ]
    for cx, cy, color in spots:
        radius = np.sqrt((hx - cx) ** 2 + (hy - cy) ** 2)
        weight = np.clip(1.0 - radius / 23.0, 0.0, 1.0) ** 1.7
        highlight = np.maximum(highlight, weight[..., None] * color)
    rgb[136:240, 128:240, :] = highlight
    return np.clip(rgb, 0.0, 1.2)


def mosaic(rgb: np.ndarray, pattern: np.ndarray) -> np.ndarray:
    ph, pw = pattern.shape
    yy, xx = np.indices(rgb.shape[:2])
    channels = pattern[yy % ph, xx % pw]
    sampled = np.take_along_axis(rgb, channels[..., None], axis=2)[..., 0]
    normalized = np.clip(sampled, 0.0, 1.0)
    return np.rint(CORPUS_BLACK + normalized * (CORPUS_WHITE - CORPUS_BLACK)).astype("<u2")


def rational(values: list[float], scale: int = 1_000_000) -> tuple[int, ...]:
    output: list[int] = []
    for value in values:
        output.extend((int(round(value * scale)), scale))
    return tuple(output)


def write_dng(path: Path, pattern: np.ndarray, make: str, model: str) -> None:
    raw = mosaic(build_scene(), pattern)
    ph, pw = pattern.shape
    # XYZ D65 -> synthetic camera RGB. This is the inverse direction required
    # by DNG ColorMatrix1; the sensor itself is deliberately idealized.
    xyz_to_camera = [
        3.2404542, -1.5371385, -0.4985314,
        -0.9692660, 1.8760108, 0.0415560,
        0.0556434, -0.2040259, 1.0572252,
    ]
    tags = [
        (271, "s", 0, make, False),
        (272, "s", 0, model, False),
        (274, "H", 1, 1, False),
        (33421, "H", 2, (ph, pw), False),
        (33422, "B", ph * pw, tuple(int(v) for v in pattern.flat), False),
        (50706, "B", 4, (1, 4, 0, 0), False),
        (50707, "B", 4, (1, 3, 0, 0), False),
        (50708, "s", 0, model, False),
        (50710, "B", 3, (0, 1, 2), False),
        (50711, "H", 1, 1, False),
        (50714, "H", 1, CORPUS_BLACK, False),
        (50717, "I", 1, CORPUS_WHITE, False),
        (50718, "2I", 2, (1, 1, 1, 1), False),
        (50719, "I", 2, (0, 0), False),
        (50720, "I", 2, (CORPUS_WIDTH, CORPUS_HEIGHT), False),
        (50721, "2i", 9, rational(xyz_to_camera), False),
        (50728, "2I", 3, (1, 1, 1, 1, 1, 1), False),
        (50730, "2i", 1, (0, 1), False),
        (50778, "H", 1, 21, False),
        (50829, "I", 4, (0, 0, CORPUS_HEIGHT, CORPUS_WIDTH), False),
    ]
    import tifffile

    tifffile.imwrite(
        path,
        raw,
        dtype=np.uint16,
        photometric=32803,
        metadata=None,
        compression=None,
        rowsperstrip=CORPUS_HEIGHT,
        extratags=tags,
        byteorder="<",
    )


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_corpus(_args: argparse.Namespace) -> int:
    """Regenerate the checked-in CC0 Bayer and X-Trans DNG fixtures."""
    if np is None:
        print("error: numpy is required for corpus generation", file=sys.stderr)
        return 2
    raw_root = ROOT / "regression/raw"
    raw_root.mkdir(parents=True, exist_ok=True)
    fixtures = [
        ("synthetic-bayer.dng", CORPUS_BAYER, "AuRaw", "AuRaw Synthetic Bayer"),
        ("synthetic-xtrans.dng", CORPUS_XTRANS, "FUJIFILM", "AuRaw Synthetic X-Trans"),
    ]
    for name, pattern, make, model in fixtures:
        path = raw_root / name
        write_dng(path, pattern, make, model)
        print(f"{digest(path)}  {path.relative_to(ROOT / 'regression')}")
    return 0


class DevCommandError(RuntimeError):
    """An actionable command failure with a stable process exit code."""

    def __init__(self, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def fail(message: str, exit_code: int = 1) -> NoReturn:
    """Raise a command failure that ``main`` can report consistently."""
    raise DevCommandError(message, exit_code)


def command_list(parts: Sequence[str | os.PathLike[str]]) -> list[str]:
    """Return a subprocess-safe argument list."""
    return [os.fspath(part) for part in parts]


def run_process(
    parts: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    check: bool = True,
    capture_output: bool = False,
    text: bool = False,
    stdout: int | None = None,
    stderr: int | None = None,
) -> subprocess.CompletedProcess[str] | subprocess.CompletedProcess[bytes]:
    """Run one explicit subprocess command and translate launch failures."""
    command = command_list(parts)
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=env,
            check=check,
            capture_output=capture_output,
            text=text,
            stdout=stdout,
            stderr=stderr,
        )
    except OSError as error:
        fail(f"unable to execute {command[0]}: {error}")


def captured_text(
    parts: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    check: bool = True,
    stderr: int | None = None,
) -> str:
    """Run a command and return stripped UTF-8-compatible text output."""
    completed = run_process(
        parts,
        cwd=cwd,
        env=env,
        check=check,
        stdout=subprocess.PIPE,
        stderr=stderr,
        text=True,
    )
    assert isinstance(completed.stdout, str)
    return completed.stdout.strip()


def require_executable(name: str, message: str | None = None) -> str:
    """Resolve an executable or fail with a clear dependency message."""
    executable = shutil.which(name)
    if executable is None:
        fail(message or f"{name} is required")
    return executable


def remove_path(path: Path) -> None:
    """Remove a file, symlink, or directory if it exists."""
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)


def rooted_path(path: str | os.PathLike[str]) -> Path:
    """Resolve a path relative to the repository, matching the old shell runner."""
    candidate = Path(path).expanduser()
    return candidate if candidate.is_absolute() else ROOT / candidate


def require_file(path: Path, message: str | None = None) -> None:
    """Require a regular file produced by a build step."""
    if not path.is_file():
        fail(message or f"required file was not produced: {path}")


def read_first_property(path: Path, key: str) -> str:
    """Read a required property from a key=value file."""
    try:
        value = parse_properties(path)[key]
    except (KeyError, OSError, ValueError) as error:
        fail(f"cannot read {key} from {relative_display(path)}: {error}")
    if not value:
        fail(f"{relative_display(path)} contains an empty {key} value")
    return value


def android_sdk_root() -> Path | None:
    """Resolve the Android SDK from the environment or local.properties."""
    configured = os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME")
    if configured:
        return rooted_path(configured)

    local_properties = ROOT / "android/local.properties"
    if local_properties.is_file():
        try:
            configured = parse_properties(local_properties).get("sdk.dir")
        except (OSError, ValueError) as error:
            fail(f"cannot read {relative_display(local_properties)}: {error}")
        if configured:
            os.environ["ANDROID_SDK_ROOT"] = configured
            return rooted_path(configured)
    return None


def android_ndk_root(expected_version: str, *, require_toolchain: bool = True) -> Path:
    """Resolve and validate the pinned Android NDK."""
    configured = os.environ.get("ANDROID_NDK_HOME") or os.environ.get("ANDROID_NDK_ROOT")
    sdk = android_sdk_root()
    ndk = rooted_path(configured) if configured else None
    if ndk is None and sdk is not None:
        ndk = sdk / "ndk" / expected_version

    if ndk is None:
        fail("Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT).")
    if require_toolchain and not (ndk / "build/cmake/android.toolchain.cmake").is_file():
        fail("Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT).")
    source_properties = ndk / "source.properties"
    if not source_properties.is_file():
        fail(f"Android NDK not found at {ndk}")

    revision = read_first_property(source_properties, "Pkg.Revision")
    if revision != expected_version:
        fail(
            f"Android NDK {expected_version} is required, found {revision or 'unknown'} at {ndk}"
        )
    return ndk


def ndk_host_root(ndk: Path) -> Path:
    """Return the selected NDK LLVM host-tool directory."""
    prebuilt = ndk / "toolchains/llvm/prebuilt"
    candidates = (
        sorted(path for path in prebuilt.iterdir() if path.is_dir())
        if prebuilt.is_dir()
        else []
    )
    if not candidates:
        fail(f"The selected NDK has no LLVM toolchain: {ndk}")
    return candidates[0]


def file_contains_line(path: Path, expected: str) -> bool:
    """Return whether a UTF-8 file contains exactly one expected line."""
    try:
        return expected in path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return False


def directory_has_xml(path: Path) -> bool:
    """Return whether a directory contains at least one XML file."""
    return path.is_dir() and any(candidate.is_file() for candidate in path.rglob("*.xml"))


def archive_cache_valid(source: Path, marker_file: str, expected_digest: str) -> bool:
    """Check the source sentinel and recorded archive digest."""
    marker = source / marker_file
    digest_file = source / ".auraw-archive-sha256"
    try:
        return (
            marker.is_file()
            and digest_file.read_text(encoding="utf-8").strip() == expected_digest
        )
    except (OSError, UnicodeError):
        return False


class HttpsOnlyRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject redirects that would downgrade a verified download to HTTP."""

    def redirect_request(
        self,
        request: urllib.request.Request,
        file_pointer: object,
        code: int,
        message: str,
        headers: object,
        new_url: str,
    ) -> urllib.request.Request | None:
        if not new_url.startswith("https://"):
            raise urllib.error.URLError(f"refusing non-HTTPS redirect: {new_url}")
        return super().redirect_request(
            request, file_pointer, code, message, headers, new_url
        )


def _urllib_download(url: str, destination: Path, *, timeout: float) -> None:
    context = ssl.create_default_context()
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    opener = urllib.request.build_opener(
        HttpsOnlyRedirectHandler(),
        urllib.request.HTTPSHandler(context=context),
    )
    request = urllib.request.Request(url, headers={"User-Agent": "AuRaw-dev/1"})
    with (
        opener.open(request, timeout=min(timeout, 30.0)) as response,
        destination.open("wb") as output,
    ):
        if not response.geturl().startswith("https://"):
            fail(f"refusing non-HTTPS redirect: {response.geturl()}", 2)
        shutil.copyfileobj(response, output)


def download_https(
    url: str,
    destination: Path,
    *,
    attempts: int,
    timeout: float,
    retry_delay: float = 3.0,
) -> None:
    """Download an HTTPS URL with retries and an optional curl fallback."""
    if not url.startswith("https://"):
        fail(f"refusing non-HTTPS download: {url}", 2)

    last_error: Exception | None = None
    curl = shutil.which("curl")
    urllib_attempts = 1 if curl is not None else attempts
    urllib_timeout = min(timeout, 5.0) if curl is not None else timeout
    for attempt in range(urllib_attempts):
        try:
            _urllib_download(url, destination, timeout=urllib_timeout)
            return
        except (OSError, urllib.error.URLError) as error:
            last_error = error
            destination.unlink(missing_ok=True)
            if attempt + 1 < urllib_attempts:
                time.sleep(retry_delay)

    if curl is not None:
        completed = run_process(
            [
                curl,
                "--proto",
                "=https",
                "--tlsv1.2",
                "--http1.1",
                "--fail",
                "--location",
                "--show-error",
                "--retry",
                str(max(attempts - 1, 0)),
                "--retry-all-errors",
                "--retry-delay",
                str(int(retry_delay)),
                "--connect-timeout",
                "30",
                "--max-time",
                str(max(1, int(timeout))),
                url,
                "-o",
                destination,
            ],
            check=False,
        )
        if completed.returncode == 0:
            return
        destination.unlink(missing_ok=True)

    fail(f"download failed for {url}: {last_error or 'unknown error'}")


def download_text_https(url: str, *, attempts: int, timeout: float) -> str:
    """Download UTF-8 checksum text over HTTPS."""
    with tempfile.TemporaryDirectory(prefix="auraw-checksum-") as temporary:
        target = Path(temporary) / "checksum.txt"
        download_https(url, target, attempts=attempts, timeout=timeout)
        try:
            return target.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            fail(f"cannot read checksum response from {url}: {error}")


def verify_digest(path: Path, algorithm: str, expected: str, *, report: bool = True) -> bool:
    """Verify one file digest and optionally emit stable diagnostics."""
    digest_object = hashlib.new(algorithm)
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest_object.update(chunk)
    except OSError as error:
        if report:
            print(f"{algorithm} checksum could not be read for {path}: {error}", file=sys.stderr)
        return False

    actual = digest_object.hexdigest()
    if actual == expected:
        return True
    if report:
        print(f"{algorithm} checksum mismatch for {path}", file=sys.stderr)
        print(f"expected: {expected}", file=sys.stderr)
        print(f"actual:   {actual}", file=sys.stderr)
    return False


def extract_tar_strip_one(archive: Path, destination: Path) -> None:
    """Extract a tar archive after removing its top-level directory."""
    with tempfile.TemporaryDirectory(prefix="auraw-archive-") as temporary:
        extraction_root = Path(temporary)
        try:
            with tarfile.open(archive, mode="r:*") as bundle:
                try:
                    bundle.extractall(extraction_root, filter="data")
                except TypeError:  # Python versions before extraction filters.
                    root = extraction_root.resolve()
                    for member in bundle.getmembers():
                        target = (extraction_root / member.name).resolve()
                        if target != root and root not in target.parents:
                            fail(f"archive contains an unsafe path: {member.name}")
                        if member.issym() or member.islnk():
                            link = Path(member.linkname)
                            if link.is_absolute() or ".." in link.parts:
                                fail(f"archive contains an unsafe link: {member.name}")
                    bundle.extractall(extraction_root)
        except (OSError, tarfile.TarError) as error:
            fail(f"cannot extract {archive}: {error}")

        entries = list(extraction_root.iterdir())
        if len(entries) != 1 or not entries[0].is_dir():
            fail(f"archive does not contain one top-level directory: {archive}")
        source_root = entries[0]
        destination.mkdir(parents=True, exist_ok=True)
        for entry in source_root.iterdir():
            target = destination / entry.name
            if entry.is_dir() and not entry.is_symlink():
                shutil.copytree(entry, target, symlinks=True)
            elif entry.is_symlink():
                target.symlink_to(os.readlink(entry), target_is_directory=entry.is_dir())
            else:
                shutil.copy2(entry, target)


def ensure_archive_source(
    source: Path,
    marker_file: str,
    url: str,
    expected_digest: str,
    *,
    temporary_prefix: str,
) -> None:
    """Populate one pinned third-party source tree from a verified archive."""
    if archive_cache_valid(source, marker_file, expected_digest):
        return

    remove_path(source)
    source.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        prefix=temporary_prefix,
        suffix=".archive",
        dir=source.parent,
        delete=False,
    ) as handle:
        archive = Path(handle.name)
    try:
        download_https(url, archive, attempts=4, timeout=900)
        if not verify_digest(archive, "sha256", expected_digest):
            fail(f"downloaded archive failed SHA-256 verification: {url}")
        extract_tar_strip_one(archive, source)
        (source / ".auraw-archive-sha256").write_text(expected_digest + "\n", encoding="utf-8")
    except Exception:
        remove_path(source)
        raise
    finally:
        archive.unlink(missing_ok=True)


def compact_json(value: object) -> str:
    """Serialize build contracts identically to the previous shell commands."""
    return json.dumps(value, separators=(",", ":"))


def android_abi_config(abi: str, api: int = 26) -> dict[str, str]:
    """Return compiler/build metadata for a supported Android ABI."""
    configs = {
        "arm64-v8a": {
            "clang_target": f"aarch64-linux-android{api}",
            "autoconf_host": "aarch64-linux-android",
            "meson_cpu_family": "aarch64",
            "meson_cpu": "aarch64",
            "cxx_triple": "aarch64-linux-android",
        },
        "armeabi-v7a": {
            "clang_target": f"armv7a-linux-androideabi{api}",
            "autoconf_host": "arm-linux-androideabi",
            "meson_cpu_family": "arm",
            "meson_cpu": "armv7",
            "cxx_triple": "arm-linux-androideabi",
        },
        "x86": {
            "clang_target": f"i686-linux-android{api}",
            "autoconf_host": "i686-linux-android",
            "meson_cpu_family": "x86",
            "meson_cpu": "i686",
            "cxx_triple": "i686-linux-android",
        },
        "x86_64": {
            "clang_target": f"x86_64-linux-android{api}",
            "autoconf_host": "x86_64-linux-android",
            "meson_cpu_family": "x86_64",
            "meson_cpu": "x86_64",
            "cxx_triple": "x86_64-linux-android",
        },
    }
    try:
        return configs[abi]
    except KeyError:
        fail(
            f"Unsupported ABI '{abi}' (use arm64-v8a, armeabi-v7a, x86, or x86_64)",
            2,
        )


def command_build_android_lensfun(args: argparse.Namespace) -> int:
    """Build pinned Lensfun, GLib, and libiconv for one Android ABI."""
    abi = args.abi
    api = 26
    lensfun_version = "0.3.4"
    lensfun_revision = "101c745e847a5de4a1e569a94368ce2027198598"
    lensfun_digest = "a11cbe6aeec657839540448b253217c25d20b7a45b6aebfef406f7239933c7a6"
    iconv_version = "1.17"
    iconv_digest = "8f74213b56238c85a50a5329f77e06198771e70dd9a739779f4c02f65d971313"
    glib_version = "2.78.6"
    glib_digest = "244854654dd82c7ebcb2f8e246156d2a05eb9cd1ad07ed7a779659b4602c9fae"
    meson_version = "1.7.0"
    meson_digest = "ae3f12953045f3c7c60e27f2af1ad862f14dee125b4ed9bcb8a842a5080dbf85"
    setuptools_version = "83.0.0"
    setuptools_digest = "29b23c360f22f414dc7336bb39178cc7bcbf6021ed2733cde173f09dba19abb3"
    expected_ndk = "28.2.13676358"

    abi_config = android_abi_config(abi, api)
    ndk = android_ndk_root(expected_ndk)
    ndk_revision = read_first_property(ndk / "source.properties", "Pkg.Revision")
    ndk_host = ndk_host_root(ndk)
    clang = ndk_host / "bin" / f"{abi_config['clang_target']}-clang"
    if not os.access(clang, os.X_OK):
        fail(f"The selected NDK has no compiler for {abi}: {ndk}")

    require_executable("ninja", "Ninja is required to build Android Lensfun.")
    sdk = android_sdk_root()
    sdk_cmake = sdk / "cmake/3.22.1/bin/cmake" if sdk else None
    if sdk_cmake is not None and os.access(sdk_cmake, os.X_OK):
        cmake = os.fspath(sdk_cmake)
    else:
        cmake = require_executable("cmake", "CMake is required to build Android Lensfun.")

    src_root = ROOT / "android/native/src"
    glib_source = src_root / f"glib-{glib_version}"
    iconv_source = src_root / f"libiconv-{iconv_version}"
    lensfun_source = src_root / f"lensfun-{lensfun_version}"
    build_root = ROOT / "android/native/build"
    iconv_build = build_root / f"libiconv-{abi}"
    glib_build = build_root / f"glib-{abi}"
    lensfun_build = build_root / f"lensfun-{abi}"
    install_dir = ROOT / f"android/native/lensfun/{abi}"
    cross_file = build_root / f"glib-{abi}.cross"
    tools_root = ROOT / "android/native/tools"
    meson_venv = tools_root / f"meson-{meson_version}"
    meson = meson_venv / "bin/meson"
    venv_python = meson_venv / "bin/python"
    src_root.mkdir(parents=True, exist_ok=True)
    build_root.mkdir(parents=True, exist_ok=True)
    tools_root.mkdir(parents=True, exist_ok=True)

    build_key = (
        f"Lensfun={lensfun_version}@{lensfun_revision} glib={glib_version} "
        f"iconv={iconv_version} abi={abi} api={api} ndk={ndk_revision}"
    )
    cached_files = (
        install_dir / "include/lensfun/lensfun.h",
        install_dir / "lib/liblensfun.a",
        install_dir / "lib/libiconv.a",
        install_dir / "lib/libglib-2.0.a",
        install_dir / "lib/libintl.a",
    )
    if (
        os.environ.get("AURAW_REBUILD_LENSFUN", "0") != "1"
        and all(path.is_file() for path in cached_files)
        and directory_has_xml(install_dir / "apk-assets/lensfun")
        and file_contains_line(install_dir / ".auraw-build", build_key)
    ):
        print(f"Using cached Lensfun {lensfun_version} for {abi} in {install_dir}")
        return 0

    meson_ready = False
    if os.access(meson, os.X_OK) and venv_python.is_file():
        version = captured_text([meson, "--version"], check=False, stderr=subprocess.DEVNULL)
        distutils_check = run_process(
            [venv_python, "-c", "import distutils.version"],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        meson_ready = version == meson_version and distutils_check.returncode == 0
    if not meson_ready:
        remove_path(meson_venv)
        completed = run_process([sys.executable, "-m", "venv", meson_venv], check=False)
        if completed.returncode != 0:
            fail("Python venv support is required to bootstrap Android Lensfun's build tool.")
        requirements = tools_root / "meson-requirements.txt"
        requirements.write_text(
            f"meson=={meson_version} --hash=sha256:{meson_digest}\n"
            f"setuptools=={setuptools_version} --hash=sha256:{setuptools_digest}\n",
            encoding="utf-8",
        )
        completed = run_process(
            [
                venv_python,
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "--no-cache-dir",
                "--no-deps",
                "--only-binary=:all:",
                "--require-hashes",
                "-r",
                requirements,
            ],
            check=False,
        )
        if completed.returncode != 0:
            remove_path(meson_venv)
            fail("Could not bootstrap Meson for Android Lensfun.")

    ensure_archive_source(
        glib_source,
        "meson.build",
        f"https://download.gnome.org/sources/glib/2.78/glib-{glib_version}.tar.xz",
        glib_digest,
        temporary_prefix=".auraw-native.",
    )
    ensure_archive_source(
        iconv_source,
        "configure",
        f"https://ftp.gnu.org/pub/gnu/libiconv/libiconv-{iconv_version}.tar.gz",
        iconv_digest,
        temporary_prefix=".auraw-native.",
    )
    ensure_archive_source(
        lensfun_source,
        "CMakeLists.txt",
        f"https://github.com/lensfun/lensfun/archive/{lensfun_revision}.tar.gz",
        lensfun_digest,
        temporary_prefix=".auraw-native.",
    )

    cross_file.write_text(
        "[binaries]\n"
        f"c = '{ndk_host}/bin/{abi_config['clang_target']}-clang'\n"
        f"cpp = '{ndk_host}/bin/{abi_config['clang_target']}-clang++'\n"
        f"ar = '{ndk_host}/bin/llvm-ar'\n"
        f"strip = '{ndk_host}/bin/llvm-strip'\n"
        "pkg-config = 'pkg-config'\n\n"
        "[properties]\n"
        "needs_exe_wrapper = true\n\n"
        "[built-in options]\n"
        f"c_args = ['-I{install_dir}/include']\n"
        f"cpp_args = ['-I{install_dir}/include']\n"
        f"c_link_args = ['-L{install_dir}/lib']\n"
        f"cpp_link_args = ['-L{install_dir}/lib']\n\n"
        "[host_machine]\n"
        "system = 'android'\n"
        f"cpu_family = '{abi_config['meson_cpu_family']}'\n"
        f"cpu = '{abi_config['meson_cpu']}'\n"
        "endian = 'little'\n",
        encoding="utf-8",
    )

    for path in (iconv_build, glib_build, lensfun_build, install_dir):
        remove_path(path)
    iconv_build.mkdir(parents=True, exist_ok=True)
    iconv_env = os.environ.copy()
    iconv_env.update(
        {
            "CC": os.fspath(ndk_host / "bin" / f"{abi_config['clang_target']}-clang"),
            "CXX": os.fspath(ndk_host / "bin" / f"{abi_config['clang_target']}-clang++"),
            "AR": os.fspath(ndk_host / "bin/llvm-ar"),
            "RANLIB": os.fspath(ndk_host / "bin/llvm-ranlib"),
        }
    )
    run_process(
        [
            iconv_source / "configure",
            f"--host={abi_config['autoconf_host']}",
            f"--prefix={install_dir}",
            "--disable-shared",
            "--enable-static",
        ],
        cwd=iconv_build,
        env=iconv_env,
    )
    run_process(
        ["make", f"-j{os.cpu_count() or 1}", "install"],
        cwd=iconv_build,
        env=iconv_env,
    )

    pkgconfig_dir = install_dir / "lib/pkgconfig"
    pkgconfig_dir.mkdir(parents=True, exist_ok=True)
    (pkgconfig_dir / "iconv.pc").write_text(
        f"prefix={install_dir}\n"
        "libdir=${prefix}/lib\n\n"
        "Name: iconv\n"
        "Description: GNU libiconv for Android Lensfun\n"
        f"Version: {iconv_version}\n"
        "Libs: -L${libdir} -liconv -lcharset\n",
        encoding="utf-8",
    )

    pkg_env = os.environ.copy()
    pkg_env["PKG_CONFIG_LIBDIR"] = os.fspath(pkgconfig_dir)
    pkg_env["PKG_CONFIG_PATH"] = os.fspath(pkgconfig_dir)
    run_process(
        [
            meson,
            "setup",
            glib_build,
            glib_source,
            "--cross-file",
            cross_file,
            "--wrap-mode=forcefallback",
            "--prefix",
            install_dir,
            "--libdir",
            "lib",
            "--default-library",
            "static",
            "--buildtype",
            "release",
            "-Dtests=false",
            "-Dnls=disabled",
            "-Dglib_debug=disabled",
            "-Dglib_assert=false",
            "-Dglib_checks=false",
            "-Dselinux=disabled",
            "-Dxattr=false",
            "-Dlibmount=disabled",
            "-Dman=false",
            "-Dgtk_doc=false",
        ],
        env=pkg_env,
    )
    run_process([meson, "compile", "-C", glib_build], env=pkg_env)
    run_process([meson, "install", "-C", glib_build], env=pkg_env)

    run_process(
        [
            cmake,
            "-S",
            lensfun_source,
            "-B",
            lensfun_build,
            "-GNinja",
            f"-DCMAKE_TOOLCHAIN_FILE={ndk / 'build/cmake/android.toolchain.cmake'}",
            f"-DANDROID_ABI={abi}",
            f"-DANDROID_PLATFORM=android-{api}",
            "-DANDROID_STL=c++_shared",
            "-DCMAKE_BUILD_TYPE=Release",
            f"-DCMAKE_INSTALL_PREFIX={install_dir}",
            "-DCMAKE_INSTALL_LIBDIR=lib",
            "-DCMAKE_INSTALL_DATAROOTDIR=apk-assets",
            "-DBUILD_STATIC=ON",
            "-DBUILD_TESTS=OFF",
            "-DBUILD_LENSTOOL=OFF",
            "-DBUILD_DOC=OFF",
            "-DINSTALL_PYTHON_MODULE=OFF",
            "-DINSTALL_HELPER_SCRIPTS=OFF",
            "-DBUILD_FOR_SSE=OFF",
            "-DBUILD_FOR_SSE2=OFF",
        ],
        env=pkg_env,
    )
    run_process([cmake, "--build", lensfun_build, "--target", "install", "--parallel"], env=pkg_env)

    for relative in (
        "include/lensfun/lensfun.h",
        "lib/liblensfun.a",
        "lib/libiconv.a",
        "lib/libcharset.a",
        "lib/libglib-2.0.a",
        "lib/libpcre2-8.a",
        "lib/libffi.a",
        "lib/libz.a",
        "lib/libintl.a",
    ):
        require_file(install_dir / relative)
    if not directory_has_xml(install_dir / "apk-assets/lensfun"):
        fail(f"Lensfun XML database was not installed in {install_dir / 'apk-assets/lensfun'}")
    (install_dir / ".auraw-build").write_text(build_key + "\n", encoding="utf-8")
    print(f"Lensfun {lensfun_version} and its database for {abi} installed in {install_dir}")
    return 0


def exact_cmake(expected_version: str) -> tuple[str, str]:
    """Resolve CMake and require the exact pinned base version."""
    sdk = android_sdk_root()
    sdk_cmake = sdk / f"cmake/{expected_version}/bin/cmake" if sdk else None
    if sdk_cmake is not None and os.access(sdk_cmake, os.X_OK):
        cmake = os.fspath(sdk_cmake)
    else:
        cmake = require_executable(
            "cmake", f"CMake {expected_version} is required to build LibRaw"
        )
    version_output = captured_text([cmake, "--version"])
    first_line = version_output.splitlines()[0] if version_output else ""
    version = first_line.removeprefix("cmake version ")
    base_version = version.split("-", 1)[0]
    if base_version != expected_version:
        fail(f"CMake {expected_version} is required, found {version or 'unknown'}")
    return cmake, version


def command_build_android_libraw(args: argparse.Namespace) -> int:
    """Build pinned LibRaw for one Android ABI."""
    expected_ndk = read_first_property(ROOT / "android/build-contract.properties", "ndkVersion")
    if args.print_build_contract:
        print(compact_json({"ndkVersion": expected_ndk}))
        return 0

    abi = args.abi
    api = 26
    libraw_version = "0.22.1"
    libraw_revision = "b860248a89d9082b8e0a1e202e516f46af9adb29"
    libraw_digest = "f5da1e522ea195b54b30f3ff105ef2193daa04ea165dea825b4d6fe9d886395b"
    cmake_version = "3.22.1"
    cmake_commit = "eb98e4325aef2ce85d2eb031c2ff18640ca616d3"
    cmake_digest = "3cd218bf6d1254de86e27269541277fbfc5bae57a9002ce0b46fbe2a97088b43"

    android_abi_config(abi, api)
    ndk = android_ndk_root(expected_ndk)
    ndk_revision = read_first_property(ndk / "source.properties", "Pkg.Revision")
    cmake, actual_cmake_version = exact_cmake(cmake_version)

    src_root = ROOT / "android/native/src"
    libraw_source = src_root / f"LibRaw-{libraw_version}"
    cmake_source = src_root / f"LibRaw-cmake-{cmake_commit}"
    build_dir = ROOT / f"android/native/build/libraw-{abi}"
    install_dir = ROOT / f"android/native/libraw/{abi}"
    src_root.mkdir(parents=True, exist_ok=True)

    build_key = (
        f"LibRaw={libraw_version}@{libraw_revision} cmake-files={cmake_commit} "
        f"cmake={actual_cmake_version} abi={abi} api={api} ndk={ndk_revision}"
    )
    if (
        os.environ.get("AURAW_REBUILD_LIBRAW", "0") != "1"
        and (install_dir / "include/libraw/libraw.h").is_file()
        and (install_dir / "lib/libraw.a").is_file()
        and file_contains_line(install_dir / ".auraw-build", build_key)
    ):
        print(f"Using cached LibRaw {libraw_version} for {abi} in {install_dir}")
        return 0

    ensure_archive_source(
        libraw_source,
        "libraw/libraw.h",
        f"https://github.com/LibRaw/LibRaw/archive/{libraw_revision}.tar.gz",
        libraw_digest,
        temporary_prefix=".libraw.",
    )
    ensure_archive_source(
        cmake_source,
        "CMakeLists.txt",
        f"https://github.com/LibRaw/LibRaw-cmake/archive/{cmake_commit}.tar.gz",
        cmake_digest,
        temporary_prefix=".libraw-cmake.",
    )

    shutil.copy2(cmake_source / "CMakeLists.txt", libraw_source / "CMakeLists.txt")
    remove_path(libraw_source / "cmake")
    shutil.copytree(cmake_source / "cmake", libraw_source / "cmake")

    remove_path(build_dir)
    remove_path(install_dir)
    command: list[str | os.PathLike[str]] = [
        cmake,
        "-S",
        libraw_source,
        "-B",
        build_dir,
    ]
    if shutil.which("ninja") is not None:
        command.append("-GNinja")
    command.extend(
        [
            f"-DCMAKE_TOOLCHAIN_FILE={ndk / 'build/cmake/android.toolchain.cmake'}",
            f"-DANDROID_ABI={abi}",
            f"-DANDROID_PLATFORM=android-{api}",
            "-DANDROID_STL=c++_static",
            "-DCMAKE_BUILD_TYPE=Release",
            f"-DCMAKE_INSTALL_PREFIX={install_dir}",
            "-DCMAKE_INSTALL_LIBDIR=lib",
            "-DBUILD_SHARED_LIBS=OFF",
            "-DENABLE_OPENMP=OFF",
            "-DENABLE_LCMS=OFF",
            "-DENABLE_JASPER=OFF",
            "-DENABLE_EXAMPLES=OFF",
            "-DENABLE_RAWSPEED=OFF",
            "-DENABLE_X3FTOOLS=OFF",
            "-DLIBRAW_INSTALL=ON",
            "-DLIBRAW_UNINSTALL_TARGET=OFF",
            "-DCMAKE_DISABLE_FIND_PACKAGE_JPEG=ON",
        ]
    )
    clean_env = os.environ.copy()
    for key in ("AR", "CC", "CFLAGS", "CPPFLAGS", "CXX", "CXXFLAGS", "LDFLAGS", "RANLIB"):
        clean_env.pop(key, None)
    run_process(command, env=clean_env)
    run_process([cmake, "--build", build_dir, "--target", "install", "--parallel"], env=clean_env)

    require_file(install_dir / "include/libraw/libraw.h")
    require_file(install_dir / "lib/libraw.a")
    (install_dir / ".auraw-build").write_text(build_key + "\n", encoding="utf-8")
    print(f"LibRaw {libraw_version} for {abi} installed in {install_dir}")
    return 0


def find_host_libclang(ndk_host: Path) -> Path | None:
    """Locate a host libclang directory for bindgen."""
    for candidate in (ndk_host / "lib64", ndk_host / "lib"):
        if candidate.is_dir() and any(candidate.glob("libclang.so*")):
            return candidate
    usr_lib = Path("/usr/lib")
    if usr_lib.is_dir():
        for library in usr_lib.rglob("libclang.so*"):
            if "llvm-" in library.as_posix() and library.is_file():
                return library.parent
    return None


def verify_source_revision(*, print_revision: bool) -> str:
    """Require a clean Git checkout and return HEAD."""
    git = shutil.which("git")
    if git is None:
        fail("git is required to verify the source revision")
    inside = run_process(
        [git, "-C", ROOT, "rev-parse", "--is-inside-work-tree"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if inside.returncode != 0:
        fail("release builds must run from a Git checkout")

    status = captured_text(
        [git, "-C", ROOT, "status", "--porcelain=v1", "--untracked-files=all"]
    )
    if status:
        print("release builds require a clean source tree:", file=sys.stderr)
        print(status, file=sys.stderr)
        raise DevCommandError("", 1)

    revision = captured_text([git, "-C", ROOT, "rev-parse", "--verify", "HEAD"])
    if print_revision:
        print(revision)
    return revision


def source_date_epoch(revision: str) -> str:
    """Return the commit timestamp used for reproducible release builds."""
    git = require_executable("git", "git is required to build a release")
    return captured_text([git, "-C", ROOT, "show", "-s", "--format=%ct", revision])


def release_build_environment(revision: str) -> dict[str, str]:
    """Create the sanitized environment shared by release builds."""
    env = os.environ.copy()
    env.update(
        {
            "AURAW_REQUIRE_COMMITTED_SOURCE": "1",
            "AURAW_SOURCE_REVISION": revision,
            "SOURCE_DATE_EPOCH": source_date_epoch(revision),
            "CARGO_INCREMENTAL": "0",
            "CARGO_TARGET_DIR": os.fspath(ROOT / "target"),
        }
    )
    for key in ("CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS"):
        env.pop(key, None)
    return env


def command_build_android(args: argparse.Namespace) -> int:
    """Build Android native dependencies and the AuRaw library."""
    expected_ndk = read_first_property(ROOT / "android/build-contract.properties", "ndkVersion")
    if args.print_build_contract:
        print(compact_json({"ndkVersion": expected_ndk}))
        return 0

    abi = args.abi
    profile = args.profile
    api = 26
    expected_cargo_ndk = "4.1.2"
    abi_config = android_abi_config(abi, api)
    ndk = android_ndk_root(expected_ndk)
    ndk_host = ndk_host_root(ndk)
    if not (ndk_host / "sysroot").is_dir():
        fail(f"The selected NDK has no LLVM sysroot: {ndk}")

    env = os.environ.copy()
    env["ANDROID_NDK_HOME"] = os.fspath(ndk)
    env["BINDGEN_EXTRA_CLANG_ARGS"] = (
        f"--target={abi_config['clang_target']} --sysroot={ndk_host / 'sysroot'}"
    )
    if not env.get("LIBCLANG_PATH"):
        libclang = find_host_libclang(ndk_host)
        if libclang is not None:
            env["LIBCLANG_PATH"] = os.fspath(libclang)

    require_executable(
        "cargo-ndk",
        "cargo-ndk 4.1.2 is required. Install it with: "
        "cargo install cargo-ndk --version 4.1.2 --locked",
    )
    cargo = require_executable("cargo")
    version_output = captured_text(
        [cargo, "ndk", "--version"], env=env, check=False, stderr=subprocess.DEVNULL
    )
    cargo_ndk_version = version_output.removeprefix("cargo-ndk ")
    if cargo_ndk_version != expected_cargo_ndk:
        fail(
            f"cargo-ndk {expected_cargo_ndk} is required, "
            f"found {cargo_ndk_version or 'unknown'}"
        )

    if not env.get("LIBCLANG_PATH") and shutil.which("ldconfig") is not None:
        ldconfig = run_process(
            ["ldconfig", "-p"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        output = ldconfig.stdout if isinstance(ldconfig.stdout, str) else ""
        if "libclang.so" not in output:
            print(
                "Warning: bindgen needs host libclang; install libclang-dev or set "
                "LIBCLANG_PATH if the build cannot find it.",
                file=sys.stderr,
            )

    revision: str | None = None
    if profile == "release":
        revision = verify_source_revision(print_revision=False)
        env.update(release_build_environment(revision))
        remove_path(ROOT / "android/native")
    else:
        env["CARGO_INCREMENTAL"] = "0"
        env["CARGO_TARGET_DIR"] = os.fspath(ROOT / "target")
    for key in ("CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS"):
        env.pop(key, None)

    dev_script = ROOT / "scripts/dev.py"
    run_process([sys.executable, dev_script, "build-android-libraw", abi], env=env)
    run_process([sys.executable, dev_script, "build-android-lensfun", abi], env=env)

    if profile == "release":
        cargo_profile = ["--release"]
    elif profile == "debug":
        cargo_profile = []
    else:
        fail(f"Unknown profile '{profile}' (use release or debug)", 2)

    libraw_root = ROOT / f"android/native/libraw/{abi}"
    lensfun_root = ROOT / f"android/native/lensfun/{abi}"
    env["AURAW_LIBRAW_ROOT"] = os.fspath(libraw_root)
    env["AURAW_LENSFUN_ROOT"] = os.fspath(lensfun_root)
    jni_root = ROOT / "android/app/src/main/jniLibs"
    abi_jni = jni_root / abi
    remove_path(abi_jni)
    run_process(
        [
            cargo,
            "ndk",
            "-t",
            abi,
            "-o",
            jni_root,
            "build",
            "--locked",
            *cargo_profile,
            "--package",
            "auraw-ui",
            "--lib",
            "--manifest-path",
            ROOT / "Cargo.toml",
        ],
        env=env,
    )

    cxx_runtime = ndk_host / f"sysroot/usr/lib/{abi_config['cxx_triple']}/libc++_shared.so"
    require_file(cxx_runtime)
    abi_jni.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cxx_runtime, abi_jni / "libc++_shared.so")
    require_file(abi_jni / "libauraw.so")
    require_file(abi_jni / "libc++_shared.so")
    if not directory_has_xml(lensfun_root / "apk-assets/lensfun"):
        fail(f"Lensfun XML database is missing from {lensfun_root / 'apk-assets/lensfun'}")

    if revision is not None:
        try:
            final_revision = verify_source_revision(print_revision=False)
        except DevCommandError:
            remove_path(abi_jni)
            print(
                "source changed during the build; discarded the Android native library",
                file=sys.stderr,
            )
            raise
        if final_revision != revision:
            remove_path(abi_jni)
            fail("source changed during the build; discarded the Android native library")

    print(f"Rust, LibRaw, and Lensfun Android libraries are ready for Gradle ({abi}, {profile}).")
    return 0


def command_build_linux(_args: argparse.Namespace) -> int:
    """Build revision-stamped Linux release binaries."""
    revision = verify_source_revision(print_revision=False)
    env = release_build_environment(revision)
    cargo = require_executable("cargo")
    run_process(
        [
            cargo,
            "build",
            "--locked",
            "--release",
            "--manifest-path",
            ROOT / "Cargo.toml",
        ],
        env=env,
    )
    outputs = (
        ROOT / "target/release/auraw",
        ROOT / "target/release/auraw-regression-render",
    )
    for output in outputs:
        require_file(output)

    try:
        final_revision = verify_source_revision(print_revision=False)
    except DevCommandError:
        for output in outputs:
            output.unlink(missing_ok=True)
        print("source changed during the build; discarded the Linux binary", file=sys.stderr)
        raise
    if final_revision != revision:
        for output in outputs:
            output.unlink(missing_ok=True)
        fail("source changed during the build; discarded the Linux binary")

    print(f"Built AuRaw from {revision}")
    return 0


def run_regression_command(arguments: Sequence[str]) -> None:
    """Run one isolated regression subcommand and stop on failure."""
    run_process(
        [sys.executable, ROOT / "scripts/dev.py", "regression", *arguments],
        cwd=ROOT,
    )


def required_environment(name: str, message: str) -> str:
    """Read one required non-empty environment variable."""
    value = os.environ.get(name)
    if not value:
        fail(message)
    return value


def command_regression_suite(_args: argparse.Namespace) -> int:
    """Run the full CPU/GPU image-regression workflow."""
    manifest = rooted_path(
        os.environ.get("AURAW_REGRESSION_MANIFEST", ROOT / "regression/corpus.yaml")
    )
    thresholds = rooted_path(
        os.environ.get("AURAW_REGRESSION_THRESHOLDS", ROOT / "regression/thresholds.yaml")
    )
    reference_engines = rooted_path(
        os.environ.get(
            "AURAW_REFERENCE_ENGINES", ROOT / "regression/reference-engines.yaml"
        )
    )
    reference_engine = os.environ.get("AURAW_REFERENCE_ENGINE", "darktable")
    reference_root = rooted_path(
        os.environ.get(
            "AURAW_REFERENCE_ROOT", ROOT / f"regression/references/{reference_engine}"
        )
    )
    output_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_OUTPUT_ROOT", ROOT / "regression/candidates")
    )
    report_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_REPORT_ROOT", ROOT / "regression/reports")
    )
    cpu_command = required_environment(
        "AURAW_CPU_RENDER_COMMAND",
        "Set AURAW_CPU_RENDER_COMMAND with {raw} and {output} placeholders",
    )
    gpu_command = required_environment(
        "AURAW_GPU_RENDER_COMMAND",
        "Set AURAW_GPU_RENDER_COMMAND with {raw} and {output} placeholders",
    )

    run_regression_command(["validate-corpus", "--manifest", os.fspath(manifest), "--verify-files"])
    run_regression_command(
        ["validate-reference-engines", "--config", os.fspath(reference_engines)]
    )
    for backend, template in (("cpu", cpu_command), ("gpu", gpu_command)):
        run_regression_command(
            [
                "render",
                "--manifest",
                os.fspath(manifest),
                "--backend",
                backend,
                "--command-template",
                template,
                "--output-root",
                os.fspath(output_root / backend),
                "--repeat",
                "2",
            ]
        )

    for backend, maximum in (
        ("cpu", os.environ.get("AURAW_CPU_DETERMINISM_MAX_ABS", "0")),
        ("gpu", os.environ.get("AURAW_GPU_DETERMINISM_MAX_ABS", "0")),
    ):
        run_regression_command(
            [
                "determinism",
                "--manifest",
                os.fspath(manifest),
                "--backend",
                backend,
                "--run-a",
                os.fspath(output_root / backend / "run-1"),
                "--run-b",
                os.fspath(output_root / backend / "run-2"),
                "--max-abs",
                maximum,
                "--report",
                os.fspath(report_root / f"{backend}-determinism.json"),
            ]
        )

    for backend in ("cpu", "gpu"):
        run_regression_command(
            [
                "compare",
                "--manifest",
                os.fspath(manifest),
                "--thresholds",
                os.fspath(thresholds),
                "--reference-root",
                os.fspath(reference_root),
                "--candidate-root",
                os.fspath(output_root / backend / "run-1"),
                "--backend",
                backend,
                "--reference-engine",
                reference_engine,
                "--reference-engines",
                os.fspath(reference_engines),
                "--report-dir",
                os.fspath(report_root / f"{backend}-vs-{reference_engine}"),
            ]
        )

    run_regression_command(
        [
            "cpu-gpu",
            "--manifest",
            os.fspath(manifest),
            "--thresholds",
            os.fspath(thresholds),
            "--cpu-root",
            os.fspath(output_root / "cpu/run-1"),
            "--gpu-root",
            os.fspath(output_root / "gpu/run-1"),
            "--report-dir",
            os.fspath(report_root / "cpu-gpu"),
        ]
    )
    return 0


def command_smoke_regression(_args: argparse.Namespace) -> int:
    """Run the deterministic regression-renderer smoke gate."""
    renderer = rooted_path(
        os.environ.get(
            "AURAW_REGRESSION_RENDERER", ROOT / "target/debug/auraw-regression-render"
        )
    )
    output_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_SMOKE_DIR", ROOT / "target/regression-smoke")
    )
    for run_number in (1, 2):
        (output_root / f"run-{run_number}").mkdir(parents=True, exist_ok=True)

    run_regression_command(
        [
            "validate-corpus",
            "--manifest",
            os.fspath(ROOT / "regression/corpus.yaml"),
            "--verify-files",
        ]
    )
    run_regression_command(
        [
            "validate-reference-engines",
            "--config",
            os.fspath(ROOT / "regression/reference-engines.yaml"),
        ]
    )

    scenes = ("synthetic-bayer-multitarget", "synthetic-xtrans-multitarget")
    for run_number in (1, 2):
        for scene in scenes:
            raw_name = scene.removesuffix("-multitarget") + ".dng"
            run_process(
                [
                    renderer,
                    "--backend",
                    "gpu",
                    "--input",
                    ROOT / "regression/raw" / raw_name,
                    "--output",
                    output_root / f"run-{run_number}/{scene}.npz",
                ]
            )

    run_regression_command(
        [
            "determinism",
            "--manifest",
            os.fspath(ROOT / "regression/corpus.yaml"),
            "--backend",
            "gpu",
            "--run-a",
            os.fspath(output_root / "run-1"),
            "--run-b",
            os.fspath(output_root / "run-2"),
            "--max-abs",
            "0",
            "--report",
            os.fspath(output_root / "determinism.json"),
        ]
    )

    regression_root = ROOT / "regression"
    sys.path.insert(0, os.fspath(regression_root))
    try:
        from iqr.io import load_linear_image

        for path in sorted((output_root / "run-1").glob("*.npz")):
            image = load_linear_image(path, color_space="linear-rec2020-d65")
            if image.rgb.shape != (256, 256, 3):
                fail(f"unexpected shape for {path}: {image.rgb.shape}")
            if image.metadata.get("renderer") != "auraw-regression-render":
                fail(f"missing renderer metadata in {path}")
            print(f"validated {path.name}: {image.rgb.shape}, {image.rgb.dtype}")
    finally:
        try:
            sys.path.remove(os.fspath(regression_root))
        except ValueError:
            pass
    return 0


def parse_expected_digest(expected_source: str) -> tuple[str, str]:
    """Resolve a literal digest or an HTTPS SHA-256 checksum document."""
    if expected_source.startswith("https://"):
        checksum_text = download_text_https(expected_source, attempts=9, timeout=300)
        match = re.search(r"[0-9a-fA-F]{64}", checksum_text)
        algorithm = "sha256"
        expected = match.group(0) if match else ""
    elif expected_source.startswith("sha256:"):
        algorithm = "sha256"
        expected = expected_source.removeprefix("sha256:")
    elif expected_source.startswith("sha512:"):
        algorithm = "sha512"
        expected = expected_source.removeprefix("sha512:")
    else:
        algorithm = "sha256"
        expected = expected_source

    if not expected or not re.fullmatch(r"[0-9a-fA-F]+", expected):
        fail("invalid checksum value", 2)
    required_length = 64 if algorithm == "sha256" else 128
    if len(expected) != required_length:
        label = "SHA-256" if algorithm == "sha256" else "SHA-512"
        fail(f"{label} must contain {required_length} hex digits", 2)
    return algorithm, expected.lower()


def command_verified_download(args: argparse.Namespace) -> int:
    """Download one HTTPS artifact and atomically verify its digest."""
    url = args.url
    if not url.startswith("https://"):
        fail(f"refusing non-HTTPS download: {url}", 2)
    algorithm, expected = parse_expected_digest(args.expected_digest)
    output = rooted_path(args.output)

    if output.is_file() and verify_digest(output, algorithm, expected, report=False):
        print(f"verified cached download: {output}")
        return 0

    temporary = output.with_name(f"{output.name}.download.{os.getpid()}")
    previous_mode: int | None = None
    try:
        if output.exists():
            previous_mode = stat.S_IMODE(output.stat().st_mode)
        temporary.unlink(missing_ok=True)
        download_https(url, temporary, attempts=9, timeout=900)
        if not verify_digest(temporary, algorithm, expected):
            return 1
        if previous_mode is not None:
            temporary.chmod(previous_mode)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    return 0


def command_verify_android_16kb(args: argparse.Namespace) -> int:
    """Verify ELF LOAD alignment and APK zip alignment for 16 KB pages."""
    contract = parse_properties(ROOT / "android/build-contract.properties")
    try:
        expected_ndk = contract["ndkVersion"]
        build_tools_version = contract["buildToolsVersion"]
    except KeyError as error:
        fail(f"missing Android build-contract property: {error.args[0]}")

    if args.print_build_contract:
        print(
            compact_json(
                {
                    "ndkVersion": expected_ndk,
                    "buildToolsVersion": build_tools_version,
                }
            )
        )
        return 0

    apk = args.apk or ROOT / "android/app/build/outputs/apk/debug/app-debug.apk"
    apk = rooted_path(apk)
    if not apk.is_file():
        fail(f"APK not found: {apk}")

    sdk = android_sdk_root()
    if sdk is None:
        fail("Android SDK not found. Set ANDROID_SDK_ROOT (or ANDROID_HOME).")
    ndk = android_ndk_root(expected_ndk, require_toolchain=False)
    ndk_host = ndk_host_root(ndk)
    objdump = ndk_host / "bin/llvm-objdump"
    zipalign = sdk / f"build-tools/{build_tools_version}/zipalign"
    if not os.access(objdump, os.X_OK):
        fail(f"llvm-objdump not found: {objdump}")
    if not os.access(zipalign, os.X_OK):
        fail(f"zipalign {build_tools_version} not found: {zipalign}")

    found_64 = False
    with tempfile.TemporaryDirectory(prefix="auraw-16kb-") as temporary:
        extraction_root = Path(temporary)
        try:
            with zipfile.ZipFile(apk) as archive:
                for info in archive.infolist():
                    parts = Path(info.filename).parts
                    if (
                        len(parts) == 3
                        and parts[0] == "lib"
                        and info.filename.endswith(".so")
                        and not info.is_dir()
                    ):
                        target = extraction_root.joinpath(*parts)
                        target.parent.mkdir(parents=True, exist_ok=True)
                        with archive.open(info) as source, target.open("wb") as destination:
                            shutil.copyfileobj(source, destination)
        except (OSError, zipfile.BadZipFile) as error:
            fail(f"cannot extract native libraries from {apk}: {error}")

        for abi in ("arm64-v8a", "x86_64"):
            library_dir = extraction_root / "lib" / abi
            if not library_dir.is_dir():
                continue
            for library in sorted(library_dir.glob("*.so")):
                found_64 = True
                completed = run_process(
                    [objdump, "-p", library],
                    check=False,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                output = completed.stdout if isinstance(completed.stdout, str) else ""
                alignments = [
                    int(match.group(1))
                    for line in output.splitlines()
                    if "LOAD" in line
                    for match in [re.search(r"align 2\*\*(\d+)", line)]
                    if match is not None
                ]
                if completed.returncode != 0 or not alignments:
                    fail(f"Could not read ELF LOAD alignment from {library}")
                if any(alignment < 14 for alignment in alignments):
                    print(f"16 KB ELF alignment check failed: {library}", file=sys.stderr)
                    for line in output.splitlines():
                        if "LOAD" in line:
                            print(line, file=sys.stderr)
                    raise DevCommandError("", 1)
                print(f"16 KB ELF aligned: {library.relative_to(extraction_root).as_posix()}")

    if not found_64:
        print("No 64-bit native libraries found; ELF 16 KB check not applicable.")

    run_process([zipalign, "-c", "-P", "16", "-v", "4", apk])
    print(f"Android 16 KB page-size checks passed: {apk}")
    return 0


def command_verify_source_revision(_args: argparse.Namespace) -> int:
    """Require a clean Git checkout and print HEAD."""
    verify_source_revision(print_revision=True)
    return 0


def command_regression(args: argparse.Namespace) -> int:
    """Delegate to the image-quality regression framework."""
    regression_root = ROOT / "regression"
    sys.path.insert(0, str(regression_root))
    try:
        from iqr.cli import main as regression_main
        return int(regression_main(args.regression_args))
    finally:
        try:
            sys.path.remove(str(regression_root))
        except ValueError:
            pass

def build_parser() -> argparse.ArgumentParser:
    """Build the command-line parser."""
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    check_all = subparsers.add_parser("check-all", help="run source, workflow-pin, and Gradle-wrapper checks")
    check_all.set_defaults(handler=lambda _args: run_checks(ALL_CHECKS))

    check_source = subparsers.add_parser("check-source", help="check source reachability, shaders, and generated binaries")
    check_source.set_defaults(handler=lambda _args: run_checks((CHECK_SOURCE,)))

    check_workflows = subparsers.add_parser("check-workflows", help="validate immutable commit pins in CI workflows")
    check_workflows.set_defaults(handler=lambda _args: run_checks((CHECK_WORKFLOWS,)))

    check_gradle = subparsers.add_parser("check-gradle", help="validate the checked-in Gradle wrapper")
    check_gradle.set_defaults(handler=lambda _args: run_checks((CHECK_GRADLE,)))

    validate_math = subparsers.add_parser("validate-math", help="run analytical camera-profile and demosaic tests")
    validate_math.add_argument("--release", action="store_true", help="run the selected Rust tests in release mode")
    validate_math.set_defaults(handler=command_validate_math)

    bench_parser = subparsers.add_parser("bench", help="benchmark the canonical GPU regression renderer")
    bench_parser.add_argument("--renderer", type=Path, default=Path("target/release/auraw-regression-render"))
    bench_parser.add_argument("--runs", type=int, default=3)
    bench_parser.add_argument("--output", type=Path, default=Path("target/benchmark-report.json"))
    bench_parser.add_argument("--budget-file", type=Path, default=Path("benchmarks/gpu-budget.json"))
    bench_parser.add_argument("--enforce-budget", action="store_true")
    bench_parser.add_argument("--dry-run", action="store_true")
    bench_parser.set_defaults(handler=command_bench)

    icons_parser = subparsers.add_parser("icons", help="generate release icon rasters")
    icons_parser.set_defaults(handler=command_icons)

    corpus_parser = subparsers.add_parser("corpus", help="regenerate the synthetic RAW corpus")
    corpus_parser.set_defaults(handler=command_corpus)

    low_tone = subparsers.add_parser("analyze-low-tone", help="emit the Shadows/Blacks analytical response table")
    low_tone.add_argument("--csv", type=Path)
    low_tone.set_defaults(handler=command_analyze_low_tone)

    compare = subparsers.add_parser("compare-lightroom", help="compare AuRaw controls with Lightroom endpoints")
    compare.add_argument("--lightroom-dir", type=Path, required=True)
    compare.add_argument("--auraw-dir", type=Path, required=True)
    compare.add_argument("--lightroom-baseline", default="Camera NT.tif")
    compare.add_argument("--auraw-baseline", default="baseline.png")
    compare.add_argument("--lightroom-crop", type=parse_crop, default=None)
    compare.add_argument("--auraw-crop", type=parse_crop, default=None)
    compare.add_argument("--sample-step", type=int, default=4)
    compare.set_defaults(handler=command_compare_lightroom)

    regression = subparsers.add_parser("regression", help="run an image-regression framework command")
    regression.add_argument("regression_args", nargs=argparse.REMAINDER)
    regression.set_defaults(handler=command_regression)

    regression_suite = subparsers.add_parser("regression-suite", help="run the full CPU/GPU image-regression workflow")
    regression_suite.set_defaults(handler=command_regression_suite)

    smoke = subparsers.add_parser("smoke-regression", help="run the deterministic regression-renderer smoke gate")
    smoke.set_defaults(handler=command_smoke_regression)

    build_android = subparsers.add_parser("build-android", help="build Android native dependencies and AuRaw library")
    build_android.add_argument("abi", nargs="?", default="arm64-v8a")
    build_android.add_argument("profile", nargs="?", default="release")
    build_android.add_argument("--print-build-contract", action="store_true")
    build_android.set_defaults(handler=command_build_android)

    build_libraw = subparsers.add_parser("build-android-libraw", help="build pinned LibRaw for one Android ABI")
    build_libraw.add_argument("abi", nargs="?", default="arm64-v8a")
    build_libraw.add_argument("--print-build-contract", action="store_true")
    build_libraw.set_defaults(handler=command_build_android_libraw)

    build_lensfun = subparsers.add_parser("build-android-lensfun", help="build pinned Lensfun for one Android ABI")
    build_lensfun.add_argument("abi", nargs="?", default="arm64-v8a")
    build_lensfun.set_defaults(handler=command_build_android_lensfun)

    build_linux = subparsers.add_parser("build-linux", help="build a revision-stamped Linux release")
    build_linux.set_defaults(handler=command_build_linux)

    verify_android = subparsers.add_parser("verify-android-16kb", help="verify Android ELF and APK 16 KB alignment")
    verify_android.add_argument("apk", nargs="?", type=Path)
    verify_android.add_argument("--print-build-contract", action="store_true")
    verify_android.set_defaults(handler=command_verify_android_16kb)

    download = subparsers.add_parser("verified-download", help="download an HTTPS artifact and verify its digest")
    download.add_argument("url")
    download.add_argument("output")
    download.add_argument("expected_digest")
    download.set_defaults(handler=command_verified_download)

    revision = subparsers.add_parser("verify-source-revision", help="require a clean Git checkout and print HEAD")
    revision.set_defaults(handler=command_verify_source_revision)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Parse arguments and dispatch a development command."""
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.handler(args))
    except KeyboardInterrupt:
        return 130
    except subprocess.CalledProcessError as error:
        return int(error.returncode or 1)
    except DevCommandError as error:
        if str(error):
            print(f"error: {error}", file=sys.stderr)
        return error.exit_code
    except OSError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
