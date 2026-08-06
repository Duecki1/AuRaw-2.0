#!/usr/bin/env python3
"""Development and CI validation commands for AuRaw."""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass
import hashlib
from pathlib import Path
import re
import shutil
import shlex
import statistics
import stat
import subprocess
import tempfile
import time
import sys
import tomllib

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
            manifest_rust = str(manifest["package"]["rust-version"])
            pinned_rust = str(toolchain["toolchain"]["channel"])
            if normalized_rust_version(manifest_rust) != normalized_rust_version(pinned_rust):
                self.errors.append(
                    "Rust version mismatch: Cargo.toml declares "
                    f"{manifest_rust}, rust-toolchain.toml pins {pinned_rust}"
                )
        except (KeyError, OSError, ValueError, tomllib.TOMLDecodeError) as error:
            self.errors.append(f"cannot validate the pinned Rust version: {error}")

    def _validate_rust_modules(self) -> None:
        if not SRC.is_dir():
            self.errors.append("missing Rust source directory: src")
            return

        roots = [path for path in (SRC / "lib.rs", SRC / "main.rs") if path.is_file()]
        roots.extend(sorted((SRC / "bin").glob("*.rs")))
        if not roots:
            self.errors.append("no Rust crate roots found under src")
            return

        for root in roots:
            self._visit_module(root)

        for module in sorted(SRC.rglob("*.rs")):
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
        shader_dir = SRC / "shaders"
        if not shader_dir.is_dir():
            self.errors.append("missing shader source directory: src/shaders")
            return

        shader_paths = {
            path.relative_to(ROOT).as_posix() for path in shader_dir.glob("*.wgsl")
        }

        build_rs = self._read_text(ROOT / "build.rs", "build script")
        if build_rs is not None:
            watched = set(re.findall(r'"(src/shaders/[^"\\]+\.wgsl)"', build_rs))
            for path in sorted(shader_paths - watched):
                self.errors.append(f"WGSL file is not watched by build.rs: {path}")
            for path in sorted(watched - shader_paths):
                self.errors.append(f"build.rs watches a missing WGSL file: {path}")

        rust_sources: list[str] = []
        for path in SRC.rglob("*.rs"):
            source = self._read_text(path, "Rust source")
            if source is not None:
                rust_sources.append(source)
        included_names = set(
            re.findall(
                r'include_str!\("\.\./shaders/([^"\\]+\.wgsl)"\)',
                "\n".join(rust_sources),
            )
        )
        included = {f"src/shaders/{name}" for name in included_names}

        preprocessor_path = ROOT / "build_support/shader_preprocessor.rs"
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
                included.add(f"src/shaders/{template_name}")
                template_source = self._read_text(template, "shader template")
                if template_source is None:
                    continue
                for fragment in re.findall(
                    r'//\s*@include\s+"([^"\\]+\.wgsl)"', template_source
                ):
                    included.add(f"src/shaders/{fragment}")

        for path in sorted(shader_paths - included):
            self.errors.append(f"WGSL file is not included by Rust source: {path}")

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

_BUILD_ANDROID_LENSFUN_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
API=26
LENSFUN_VERSION=0.3.4
LENSFUN_REVISION=101c745e847a5de4a1e569a94368ce2027198598
LENSFUN_ARCHIVE_SHA256=a11cbe6aeec657839540448b253217c25d20b7a45b6aebfef406f7239933c7a6
ICONV_VERSION=1.17
ICONV_ARCHIVE_SHA256=8f74213b56238c85a50a5329f77e06198771e70dd9a739779f4c02f65d971313
GLIB_VERSION=2.78.6
GLIB_ARCHIVE_SHA256=244854654dd82c7ebcb2f8e246156d2a05eb9cd1ad07ed7a779659b4602c9fae
MESON_VERSION=1.7.0
MESON_WHEEL_SHA256=ae3f12953045f3c7c60e27f2af1ad862f14dee125b4ed9bcb8a842a5080dbf85
SETUPTOOLS_VERSION=83.0.0
SETUPTOOLS_WHEEL_SHA256=29b23c360f22f414dc7336bb39178cc7bcbf6021ed2733cde173f09dba19abb3
EXPECTED_NDK_VERSION=28.2.13676358

case "$ABI" in
    arm64-v8a)
        CLANG_TARGET="aarch64-linux-android$API"
        AUTOCONF_HOST=aarch64-linux-android
        MESON_CPU_FAMILY=aarch64
        MESON_CPU=aarch64
        ;;
    armeabi-v7a)
        CLANG_TARGET="armv7a-linux-androideabi$API"
        AUTOCONF_HOST=arm-linux-androideabi
        MESON_CPU_FAMILY=arm
        MESON_CPU=armv7
        ;;
    x86)
        CLANG_TARGET="i686-linux-android$API"
        AUTOCONF_HOST=i686-linux-android
        MESON_CPU_FAMILY=x86
        MESON_CPU=i686
        ;;
    x86_64)
        CLANG_TARGET="x86_64-linux-android$API"
        AUTOCONF_HOST=x86_64-linux-android
        MESON_CPU_FAMILY=x86_64
        MESON_CPU=x86_64
        ;;
    *)
        echo "Unsupported ABI '$ABI' (use arm64-v8a, armeabi-v7a, x86, or x86_64)" >&2
        exit 2
        ;;
esac

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}
if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
if [ -z "$NDK" ] && [ -n "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ]; then
    SDK=${ANDROID_SDK_ROOT:-$ANDROID_HOME}
    NDK="$SDK/ndk/$EXPECTED_NDK_VERSION"
fi
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ] \
    || [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi
NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
if [ -z "$NDK_HOST" ] || [ ! -x "$NDK_HOST/bin/${CLANG_TARGET}-clang" ]; then
    echo "The selected NDK has no compiler for $ABI: $NDK" >&2
    exit 1
fi

command -v ninja >/dev/null 2>&1 || {
    echo "Ninja is required to build Android Lensfun." >&2
    exit 1
}

SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
CMAKE=cmake
if [ -n "$SDK" ] && [ -x "$SDK/cmake/3.22.1/bin/cmake" ]; then
    CMAKE="$SDK/cmake/3.22.1/bin/cmake"
fi
command -v "$CMAKE" >/dev/null 2>&1 || {
    echo "CMake is required to build Android Lensfun." >&2
    exit 1
}

SRC_ROOT="$ROOT/android/native/src"
GLIB_SRC="$SRC_ROOT/glib-$GLIB_VERSION"
ICONV_SRC="$SRC_ROOT/libiconv-$ICONV_VERSION"
LENSFUN_SRC="$SRC_ROOT/lensfun-$LENSFUN_VERSION"
ICONV_BUILD="$ROOT/android/native/build/libiconv-$ABI"
GLIB_BUILD="$ROOT/android/native/build/glib-$ABI"
LENSFUN_BUILD="$ROOT/android/native/build/lensfun-$ABI"
INSTALL_DIR="$ROOT/android/native/lensfun/$ABI"
CROSS_FILE="$ROOT/android/native/build/glib-$ABI.cross"
TOOLS_ROOT="$ROOT/android/native/tools"
MESON_VENV="$TOOLS_ROOT/meson-$MESON_VERSION"
MESON="$MESON_VENV/bin/meson"
mkdir -p "$SRC_ROOT" "$ROOT/android/native/build" "$TOOLS_ROOT"

BUILD_KEY="Lensfun=$LENSFUN_VERSION@$LENSFUN_REVISION glib=$GLIB_VERSION iconv=$ICONV_VERSION abi=$ABI api=$API ndk=$NDK_REVISION"
if [ "${AURAW_REBUILD_LENSFUN:-0}" != 1 ] \
    && [ -f "$INSTALL_DIR/include/lensfun/lensfun.h" ] \
    && [ -f "$INSTALL_DIR/lib/liblensfun.a" ] \
    && [ -f "$INSTALL_DIR/lib/libiconv.a" ] \
    && [ -f "$INSTALL_DIR/lib/libglib-2.0.a" ] \
    && [ -f "$INSTALL_DIR/lib/libintl.a" ] \
    && [ -n "$(find "$INSTALL_DIR/apk-assets/lensfun" -type f -name '*.xml' -print -quit 2>/dev/null)" ] \
    && [ -f "$INSTALL_DIR/.auraw-build" ] \
    && grep -Fqx "$BUILD_KEY" "$INSTALL_DIR/.auraw-build"; then
    echo "Using cached Lensfun $LENSFUN_VERSION for $ABI in $INSTALL_DIR"
    exit 0
fi

# Meson is a build-time implementation detail. Keep it out of the developer's
# global Python environment and include setuptools because GLib 2.78's code
# generator still imports distutils (provided by setuptools on Python 3.12+).
if [ ! -x "$MESON" ] \
    || ! "$MESON" --version 2>/dev/null | grep -Fqx "$MESON_VERSION" \
    || ! "$MESON_VENV/bin/python" -c 'import distutils.version' 2>/dev/null; then
    rm -rf "$MESON_VENV"
    python3 -m venv "$MESON_VENV" || {
        echo "Python venv support is required to bootstrap Android Lensfun's build tool." >&2
        exit 1
    }
    MESON_REQUIREMENTS="$TOOLS_ROOT/meson-requirements.txt"
    cat > "$MESON_REQUIREMENTS" <<EOF
meson==$MESON_VERSION --hash=sha256:$MESON_WHEEL_SHA256
setuptools==$SETUPTOOLS_VERSION --hash=sha256:$SETUPTOOLS_WHEEL_SHA256
EOF
    "$MESON_VENV/bin/python" -m pip install --disable-pip-version-check --no-cache-dir \
        --no-deps --only-binary=:all: --require-hashes -r "$MESON_REQUIREMENTS" || {
        rm -rf "$MESON_VENV"
        echo "Could not bootstrap Meson for Android Lensfun." >&2
        exit 1
    }
fi

fetch_archive() {
    destination=$1
    url=$2
    expected_sha256=$3
    archive=$(mktemp "$SRC_ROOT/.auraw-native.XXXXXX")
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 --output "$archive" "$url"
    printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
    tar -xf "$archive" --strip-components=1 -C "$destination"
    rm -f "$archive"
}

if [ ! -f "$GLIB_SRC/meson.build" ] \
    || [ ! -f "$GLIB_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$GLIB_SRC/.auraw-archive-sha256")" != "$GLIB_ARCHIVE_SHA256" ]; then
    rm -rf "$GLIB_SRC"
    mkdir -p "$GLIB_SRC"
    fetch_archive "$GLIB_SRC" \
        "https://download.gnome.org/sources/glib/2.78/glib-$GLIB_VERSION.tar.xz" \
        "$GLIB_ARCHIVE_SHA256"
    printf '%s\n' "$GLIB_ARCHIVE_SHA256" > "$GLIB_SRC/.auraw-archive-sha256"
fi

if [ ! -f "$ICONV_SRC/configure" ] \
    || [ ! -f "$ICONV_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$ICONV_SRC/.auraw-archive-sha256")" != "$ICONV_ARCHIVE_SHA256" ]; then
    rm -rf "$ICONV_SRC"
    mkdir -p "$ICONV_SRC"
    fetch_archive "$ICONV_SRC" \
        "https://ftp.gnu.org/pub/gnu/libiconv/libiconv-$ICONV_VERSION.tar.gz" \
        "$ICONV_ARCHIVE_SHA256"
    printf '%s\n' "$ICONV_ARCHIVE_SHA256" > "$ICONV_SRC/.auraw-archive-sha256"
fi

if [ ! -f "$LENSFUN_SRC/CMakeLists.txt" ] \
    || [ ! -f "$LENSFUN_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$LENSFUN_SRC/.auraw-archive-sha256")" != "$LENSFUN_ARCHIVE_SHA256" ]; then
    rm -rf "$LENSFUN_SRC"
    mkdir -p "$LENSFUN_SRC"
    fetch_archive "$LENSFUN_SRC" \
        "https://github.com/lensfun/lensfun/archive/$LENSFUN_REVISION.tar.gz" \
        "$LENSFUN_ARCHIVE_SHA256"
    printf '%s\n' "$LENSFUN_ARCHIVE_SHA256" > "$LENSFUN_SRC/.auraw-archive-sha256"
fi

cat > "$CROSS_FILE" <<EOF
[binaries]
c = '$NDK_HOST/bin/${CLANG_TARGET}-clang'
cpp = '$NDK_HOST/bin/${CLANG_TARGET}-clang++'
ar = '$NDK_HOST/bin/llvm-ar'
strip = '$NDK_HOST/bin/llvm-strip'
pkg-config = 'pkg-config'

[properties]
needs_exe_wrapper = true

[built-in options]
c_args = ['-I$INSTALL_DIR/include']
cpp_args = ['-I$INSTALL_DIR/include']
c_link_args = ['-L$INSTALL_DIR/lib']
cpp_link_args = ['-L$INSTALL_DIR/lib']

[host_machine]
system = 'android'
cpu_family = '$MESON_CPU_FAMILY'
cpu = '$MESON_CPU'
endian = 'little'
EOF

rm -rf "$ICONV_BUILD" "$GLIB_BUILD" "$LENSFUN_BUILD" "$INSTALL_DIR"
mkdir -p "$ICONV_BUILD"
(
    cd "$ICONV_BUILD"
    CC="$NDK_HOST/bin/${CLANG_TARGET}-clang" \
    CXX="$NDK_HOST/bin/${CLANG_TARGET}-clang++" \
    AR="$NDK_HOST/bin/llvm-ar" \
    RANLIB="$NDK_HOST/bin/llvm-ranlib" \
    "$ICONV_SRC/configure" --host="$AUTOCONF_HOST" --prefix="$INSTALL_DIR" \
        --disable-shared --enable-static
    make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)" install
)
mkdir -p "$INSTALL_DIR/lib/pkgconfig"
cat > "$INSTALL_DIR/lib/pkgconfig/iconv.pc" <<EOF
prefix=$INSTALL_DIR
libdir=\${prefix}/lib

Name: iconv
Description: GNU libiconv for Android Lensfun
Version: $ICONV_VERSION
Libs: -L\${libdir} -liconv -lcharset
EOF

PKG_CONFIG_LIBDIR="$INSTALL_DIR/lib/pkgconfig" \
PKG_CONFIG_PATH="$INSTALL_DIR/lib/pkgconfig" \
"$MESON" setup "$GLIB_BUILD" "$GLIB_SRC" --cross-file "$CROSS_FILE" --wrap-mode=forcefallback \
    --prefix "$INSTALL_DIR" --libdir lib --default-library static --buildtype release \
    -Dtests=false -Dnls=disabled -Dglib_debug=disabled -Dglib_assert=false -Dglib_checks=false \
    -Dselinux=disabled -Dxattr=false -Dlibmount=disabled -Dman=false -Dgtk_doc=false
"$MESON" compile -C "$GLIB_BUILD"
"$MESON" install -C "$GLIB_BUILD"

PKG_CONFIG_LIBDIR="$INSTALL_DIR/lib/pkgconfig" \
PKG_CONFIG_PATH="$INSTALL_DIR/lib/pkgconfig" \
"$CMAKE" -S "$LENSFUN_SRC" -B "$LENSFUN_BUILD" -GNinja \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI="$ABI" -DANDROID_PLATFORM="android-$API" -DANDROID_STL=c++_shared \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$INSTALL_DIR" -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_INSTALL_DATAROOTDIR=apk-assets \
    -DBUILD_STATIC=ON -DBUILD_TESTS=OFF -DBUILD_LENSTOOL=OFF -DBUILD_DOC=OFF \
    -DINSTALL_PYTHON_MODULE=OFF -DINSTALL_HELPER_SCRIPTS=OFF -DBUILD_FOR_SSE=OFF -DBUILD_FOR_SSE2=OFF
"$CMAKE" --build "$LENSFUN_BUILD" --target install --parallel

test -f "$INSTALL_DIR/include/lensfun/lensfun.h"
test -f "$INSTALL_DIR/lib/liblensfun.a"
test -f "$INSTALL_DIR/lib/libiconv.a"
test -f "$INSTALL_DIR/lib/libcharset.a"
test -f "$INSTALL_DIR/lib/libglib-2.0.a"
test -f "$INSTALL_DIR/lib/libpcre2-8.a"
test -f "$INSTALL_DIR/lib/libffi.a"
test -f "$INSTALL_DIR/lib/libz.a"
test -f "$INSTALL_DIR/lib/libintl.a"
test -n "$(find "$INSTALL_DIR/apk-assets/lensfun" -type f -name '*.xml' -print -quit)"
printf '%s\n' "$BUILD_KEY" > "$INSTALL_DIR/.auraw-build"
echo "Lensfun $LENSFUN_VERSION and its database for $ABI installed in $INSTALL_DIR"
'''

_BUILD_ANDROID_LIBRAW_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
API=26
LIBRAW_VERSION=0.22.1
LIBRAW_REVISION=b860248a89d9082b8e0a1e202e516f46af9adb29
LIBRAW_ARCHIVE_SHA256=f5da1e522ea195b54b30f3ff105ef2193daa04ea165dea825b4d6fe9d886395b
BUILD_CONTRACT="$ROOT/android/build-contract.properties"
EXPECTED_NDK_VERSION=$(sed -n 's/^ndkVersion=//p' "$BUILD_CONTRACT")
EXPECTED_CMAKE_VERSION=3.22.1
LIBRAW_CMAKE_COMMIT=eb98e4325aef2ce85d2eb031c2ff18640ca616d3
LIBRAW_CMAKE_ARCHIVE_SHA256=3cd218bf6d1254de86e27269541277fbfc5bae57a9002ce0b46fbe2a97088b43

if [ "${1:-}" = "--print-build-contract" ]; then
    printf '{"ndkVersion":"%s"}\n' "$EXPECTED_NDK_VERSION"
    exit 0
fi

case "$ABI" in
    arm64-v8a|armeabi-v7a|x86|x86_64) ;;
    *)
        echo "Unsupported ABI '$ABI' (use arm64-v8a, armeabi-v7a, x86, or x86_64)" >&2
        exit 2
        ;;
esac

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}
if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
if [ -z "$NDK" ] && [ -n "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ]; then
    SDK=${ANDROID_SDK_ROOT:-$ANDROID_HOME}
    NDK="$SDK/ndk/$EXPECTED_NDK_VERSION"
fi
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ] || [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi

NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi

SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
CMAKE=cmake
if [ -n "$SDK" ] && [ -x "$SDK/cmake/$EXPECTED_CMAKE_VERSION/bin/cmake" ]; then
    CMAKE="$SDK/cmake/$EXPECTED_CMAKE_VERSION/bin/cmake"
elif ! command -v "$CMAKE" >/dev/null 2>&1; then
    echo "CMake $EXPECTED_CMAKE_VERSION is required to build LibRaw" >&2
    exit 1
fi
CMAKE_VERSION=$("$CMAKE" --version | sed -n '1s/^cmake version //p')
CMAKE_BASE_VERSION=${CMAKE_VERSION%%-*}
if [ "$CMAKE_BASE_VERSION" != "$EXPECTED_CMAKE_VERSION" ]; then
    echo "CMake $EXPECTED_CMAKE_VERSION is required, found ${CMAKE_VERSION:-unknown}" >&2
    exit 1
fi
command -v curl >/dev/null 2>&1 || {
    echo "curl is required to download LibRaw" >&2
    exit 1
}
command -v sha256sum >/dev/null 2>&1 || {
    echo "sha256sum is required to verify LibRaw sources" >&2
    exit 1
}
unset AR CC CFLAGS CPPFLAGS CXX CXXFLAGS LDFLAGS RANLIB

SRC_ROOT="$ROOT/android/native/src"
LIBRAW_SRC="$SRC_ROOT/LibRaw-$LIBRAW_VERSION"
CMAKE_SRC="$SRC_ROOT/LibRaw-cmake-$LIBRAW_CMAKE_COMMIT"
BUILD_DIR="$ROOT/android/native/build/libraw-$ABI"
INSTALL_DIR="$ROOT/android/native/libraw/$ABI"
mkdir -p "$SRC_ROOT"

BUILD_KEY="LibRaw=$LIBRAW_VERSION@$LIBRAW_REVISION cmake-files=$LIBRAW_CMAKE_COMMIT cmake=$CMAKE_VERSION abi=$ABI api=$API ndk=$NDK_REVISION"
if [ "${AURAW_REBUILD_LIBRAW:-0}" != 1 ] \
    && [ -f "$INSTALL_DIR/include/libraw/libraw.h" ] \
    && [ -f "$INSTALL_DIR/lib/libraw.a" ] \
    && [ -f "$INSTALL_DIR/.auraw-build" ] \
    && grep -Fqx "$BUILD_KEY" "$INSTALL_DIR/.auraw-build"; then
    echo "Using cached LibRaw $LIBRAW_VERSION for $ABI in $INSTALL_DIR"
    exit 0
fi

if [ ! -f "$LIBRAW_SRC/libraw/libraw.h" ] \
    || [ ! -f "$LIBRAW_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$LIBRAW_SRC/.auraw-archive-sha256")" != "$LIBRAW_ARCHIVE_SHA256" ]; then
    LIBRAW_ARCHIVE=$(mktemp "$SRC_ROOT/.libraw.XXXXXX.tar.gz")
    trap 'rm -f "${LIBRAW_ARCHIVE:-}" "${CMAKE_ARCHIVE:-}"' EXIT HUP INT TERM
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 \
        --output "$LIBRAW_ARCHIVE" \
        "https://github.com/LibRaw/LibRaw/archive/$LIBRAW_REVISION.tar.gz"
    printf '%s  %s\n' "$LIBRAW_ARCHIVE_SHA256" "$LIBRAW_ARCHIVE" | sha256sum --check --status
    rm -rf "$LIBRAW_SRC"
    mkdir -p "$LIBRAW_SRC"
    tar -xzf "$LIBRAW_ARCHIVE" --strip-components=1 -C "$LIBRAW_SRC"
    printf '%s\n' "$LIBRAW_ARCHIVE_SHA256" > "$LIBRAW_SRC/.auraw-archive-sha256"
    rm -f "$LIBRAW_ARCHIVE"
    LIBRAW_ARCHIVE=
fi

if [ ! -f "$CMAKE_SRC/CMakeLists.txt" ] \
    || [ ! -f "$CMAKE_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$CMAKE_SRC/.auraw-archive-sha256")" != "$LIBRAW_CMAKE_ARCHIVE_SHA256" ]; then
    CMAKE_ARCHIVE=$(mktemp "$SRC_ROOT/.libraw-cmake.XXXXXX.tar.gz")
    trap 'rm -f "${LIBRAW_ARCHIVE:-}" "${CMAKE_ARCHIVE:-}"' EXIT HUP INT TERM
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 \
        --output "$CMAKE_ARCHIVE" \
        "https://github.com/LibRaw/LibRaw-cmake/archive/$LIBRAW_CMAKE_COMMIT.tar.gz"
    printf '%s  %s\n' "$LIBRAW_CMAKE_ARCHIVE_SHA256" "$CMAKE_ARCHIVE" | sha256sum --check --status
    rm -rf "$CMAKE_SRC"
    mkdir -p "$CMAKE_SRC"
    tar -xzf "$CMAKE_ARCHIVE" --strip-components=1 -C "$CMAKE_SRC"
    printf '%s\n' "$LIBRAW_CMAKE_ARCHIVE_SHA256" > "$CMAKE_SRC/.auraw-archive-sha256"
    rm -f "$CMAKE_ARCHIVE"
    CMAKE_ARCHIVE=
fi

# LibRaw intentionally maintains its CMake files in a companion repository.
cp "$CMAKE_SRC/CMakeLists.txt" "$LIBRAW_SRC/CMakeLists.txt"
rm -rf "$LIBRAW_SRC/cmake"
cp -R "$CMAKE_SRC/cmake" "$LIBRAW_SRC/cmake"

GENERATOR=
if command -v ninja >/dev/null 2>&1; then
    GENERATOR="-GNinja"
fi

rm -rf "$BUILD_DIR" "$INSTALL_DIR"
"$CMAKE" -S "$LIBRAW_SRC" -B "$BUILD_DIR" $GENERATOR \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI="$ABI" \
    -DANDROID_PLATFORM="android-$API" \
    -DANDROID_STL=c++_static \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$INSTALL_DIR" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=OFF \
    -DENABLE_OPENMP=OFF \
    -DENABLE_LCMS=OFF \
    -DENABLE_JASPER=OFF \
    -DENABLE_EXAMPLES=OFF \
    -DENABLE_RAWSPEED=OFF \
    -DENABLE_X3FTOOLS=OFF \
    -DLIBRAW_INSTALL=ON \
    -DLIBRAW_UNINSTALL_TARGET=OFF \
    -DCMAKE_DISABLE_FIND_PACKAGE_JPEG=ON
"$CMAKE" --build "$BUILD_DIR" --target install --parallel

test -f "$INSTALL_DIR/include/libraw/libraw.h"
test -f "$INSTALL_DIR/lib/libraw.a"
printf '%s\n' "$BUILD_KEY" > "$INSTALL_DIR/.auraw-build"
echo "LibRaw $LIBRAW_VERSION for $ABI installed in $INSTALL_DIR"
'''

_BUILD_ANDROID_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
PROFILE=${2:-release}
API=26
BUILD_CONTRACT="$ROOT/android/build-contract.properties"
EXPECTED_NDK_VERSION=$(sed -n 's/^ndkVersion=//p' "$BUILD_CONTRACT")
EXPECTED_CARGO_NDK_VERSION=4.1.2

if [ "${1:-}" = "--print-build-contract" ]; then
    printf '{"ndkVersion":"%s"}\n' "$EXPECTED_NDK_VERSION"
    exit 0
fi

case "$ABI" in
    arm64-v8a)
        CLANG_TARGET="aarch64-linux-android$API"
        CXX_TRIPLE=aarch64-linux-android
        ;;
    armeabi-v7a)
        CLANG_TARGET="armv7a-linux-androideabi$API"
        CXX_TRIPLE=arm-linux-androideabi
        ;;
    x86)
        CLANG_TARGET="i686-linux-android$API"
        CXX_TRIPLE=i686-linux-android
        ;;
    x86_64)
        CLANG_TARGET="x86_64-linux-android$API"
        CXX_TRIPLE=x86_64-linux-android
        ;;
    *)
        echo "Unsupported ABI '$ABI' (use arm64-v8a, armeabi-v7a, x86, or x86_64)" >&2
        exit 2
        ;;
esac

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}
if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
if [ -z "$NDK" ] && [ -n "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ]; then
    SDK=${ANDROID_SDK_ROOT:-$ANDROID_HOME}
    NDK="$SDK/ndk/$EXPECTED_NDK_VERSION"
fi
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ] || [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi
export ANDROID_NDK_HOME="$NDK"

NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
if [ -z "$NDK_HOST" ] || [ ! -d "$NDK_HOST/sysroot" ]; then
    echo "The selected NDK has no LLVM sysroot: $NDK" >&2
    exit 1
fi
export BINDGEN_EXTRA_CLANG_ARGS="--target=$CLANG_TARGET --sysroot=$NDK_HOST/sysroot"

if [ -z "${LIBCLANG_PATH:-}" ]; then
    for candidate in "$NDK_HOST/lib64" "$NDK_HOST/lib"; do
        if find "$candidate" -maxdepth 1 -name 'libclang.so*' -print -quit 2>/dev/null | grep -q .; then
            export LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi
if [ -z "${LIBCLANG_PATH:-}" ]; then
    LIBCLANG_SO=$(find /usr/lib -path '*/llvm-*/lib/libclang.so*' -print -quit 2>/dev/null || true)
    if [ -n "$LIBCLANG_SO" ]; then
        LIBCLANG_PATH=$(dirname "$LIBCLANG_SO")
        export LIBCLANG_PATH
    fi
fi

command -v cargo-ndk >/dev/null 2>&1 || {
    echo "cargo-ndk $EXPECTED_CARGO_NDK_VERSION is required. Install it with: cargo install cargo-ndk --version $EXPECTED_CARGO_NDK_VERSION --locked" >&2
    exit 1
}
CARGO_NDK_VERSION=$(cargo ndk --version 2>/dev/null | sed -n 's/^cargo-ndk //p')
if [ "$CARGO_NDK_VERSION" != "$EXPECTED_CARGO_NDK_VERSION" ]; then
    echo "cargo-ndk $EXPECTED_CARGO_NDK_VERSION is required, found ${CARGO_NDK_VERSION:-unknown}" >&2
    exit 1
fi

if [ -z "${LIBCLANG_PATH:-}" ] && command -v ldconfig >/dev/null 2>&1 \
    && ! ldconfig -p 2>/dev/null | grep -q 'libclang\.so'; then
    echo "Warning: bindgen needs host libclang; install libclang-dev or set LIBCLANG_PATH if the build cannot find it." >&2
fi

if [ "$PROFILE" = release ]; then
    REVISION=$(python3 "$ROOT/scripts/dev.py" verify-source-revision)
    export AURAW_REQUIRE_COMMITTED_SOURCE=1
    export AURAW_SOURCE_REVISION="$REVISION"
    export SOURCE_DATE_EPOCH="$(git -C "$ROOT" show -s --format=%ct "$REVISION")"
    rm -rf "$ROOT/android/native"
fi
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$ROOT/target"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTFLAGS RUSTDOCFLAGS

python3 "$ROOT/scripts/dev.py" build-android-libraw "$ABI"
python3 "$ROOT/scripts/dev.py" build-android-lensfun "$ABI"

case "$PROFILE" in
    release) CARGO_PROFILE=--release ;;
    debug) CARGO_PROFILE= ;;
    *)
        echo "Unknown profile '$PROFILE' (use release or debug)" >&2
        exit 2
        ;;
esac

export AURAW_LIBRAW_ROOT="$ROOT/android/native/libraw/$ABI"
export AURAW_LENSFUN_ROOT="$ROOT/android/native/lensfun/$ABI"
rm -rf "$ROOT/android/app/src/main/jniLibs/$ABI"
# shellcheck disable=SC2086
cargo ndk -t "$ABI" -o "$ROOT/android/app/src/main/jniLibs" \
    build --locked $CARGO_PROFILE --lib --manifest-path "$ROOT/Cargo.toml"

CXX_RUNTIME="$NDK_HOST/sysroot/usr/lib/$CXX_TRIPLE/libc++_shared.so"
test -f "$CXX_RUNTIME"
cp "$CXX_RUNTIME" "$ROOT/android/app/src/main/jniLibs/$ABI/libc++_shared.so"
test -f "$ROOT/android/app/src/main/jniLibs/$ABI/libauraw.so"
test -f "$ROOT/android/app/src/main/jniLibs/$ABI/libc++_shared.so"
test -n "$(find "$AURAW_LENSFUN_ROOT/apk-assets/lensfun" -type f -name '*.xml' -print -quit)"

if [ "$PROFILE" = release ]; then
    if ! FINAL_REVISION=$(python3 "$ROOT/scripts/dev.py" verify-source-revision) \
        || [ "$FINAL_REVISION" != "$REVISION" ]; then
        rm -rf "$ROOT/android/app/src/main/jniLibs/$ABI"
        echo "source changed during the build; discarded the Android native library" >&2
        exit 1
    fi
fi

echo "Rust, LibRaw, and Lensfun Android libraries are ready for Gradle ($ABI, $PROFILE)."
'''

_BUILD_LINUX_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$#" -ne 0 ]; then
    echo "build-linux.sh does not accept Cargo overrides" >&2
    exit 2
fi
REVISION=$(python3 "$ROOT/scripts/dev.py" verify-source-revision)

export AURAW_REQUIRE_COMMITTED_SOURCE=1
export AURAW_SOURCE_REVISION="$REVISION"
export SOURCE_DATE_EPOCH="$(git -C "$ROOT" show -s --format=%ct "$REVISION")"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$ROOT/target"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTFLAGS RUSTDOCFLAGS

cargo build --locked --release --manifest-path "$ROOT/Cargo.toml"
test -f "$ROOT/target/release/auraw"
test -f "$ROOT/target/release/auraw-regression-render"

if ! FINAL_REVISION=$(python3 "$ROOT/scripts/dev.py" verify-source-revision) \
    || [ "$FINAL_REVISION" != "$REVISION" ]; then
    rm -f "$ROOT/target/release/auraw" \
        "$ROOT/target/release/auraw-regression-render"
    echo "source changed during the build; discarded the Linux binary" >&2
    exit 1
fi

printf 'Built AuRaw from %s\n' "$REVISION"
'''

_RUN_IMAGE_REGRESSION_SH = r'''#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${AURAW_REGRESSION_MANIFEST:-$ROOT/regression/corpus.yaml}"
THRESHOLDS="${AURAW_REGRESSION_THRESHOLDS:-$ROOT/regression/thresholds.yaml}"
REFERENCE_ENGINES="${AURAW_REFERENCE_ENGINES:-$ROOT/regression/reference-engines.yaml}"
REFERENCE_ENGINE="${AURAW_REFERENCE_ENGINE:-darktable}"
REFERENCE_ROOT="${AURAW_REFERENCE_ROOT:-$ROOT/regression/references/$REFERENCE_ENGINE}"
OUTPUT_ROOT="${AURAW_REGRESSION_OUTPUT_ROOT:-$ROOT/regression/candidates}"
REPORT_ROOT="${AURAW_REGRESSION_REPORT_ROOT:-$ROOT/regression/reports}"
: "${AURAW_CPU_RENDER_COMMAND:?Set AURAW_CPU_RENDER_COMMAND with {raw} and {output} placeholders}"
: "${AURAW_GPU_RENDER_COMMAND:?Set AURAW_GPU_RENDER_COMMAND with {raw} and {output} placeholders}"

python3 "$ROOT/scripts/dev.py" regression validate-corpus \
  --manifest "$MANIFEST" --verify-files
python3 "$ROOT/scripts/dev.py" regression validate-reference-engines \
  --config "$REFERENCE_ENGINES"

python3 "$ROOT/scripts/dev.py" regression render \
  --manifest "$MANIFEST" --backend cpu \
  --command-template "$AURAW_CPU_RENDER_COMMAND" \
  --output-root "$OUTPUT_ROOT/cpu" --repeat 2

python3 "$ROOT/scripts/dev.py" regression render \
  --manifest "$MANIFEST" --backend gpu \
  --command-template "$AURAW_GPU_RENDER_COMMAND" \
  --output-root "$OUTPUT_ROOT/gpu" --repeat 2

python3 "$ROOT/scripts/dev.py" regression determinism \
  --manifest "$MANIFEST" --backend cpu \
  --run-a "$OUTPUT_ROOT/cpu/run-1" --run-b "$OUTPUT_ROOT/cpu/run-2" \
  --max-abs "${AURAW_CPU_DETERMINISM_MAX_ABS:-0}" \
  --report "$REPORT_ROOT/cpu-determinism.json"

python3 "$ROOT/scripts/dev.py" regression determinism \
  --manifest "$MANIFEST" --backend gpu \
  --run-a "$OUTPUT_ROOT/gpu/run-1" --run-b "$OUTPUT_ROOT/gpu/run-2" \
  --max-abs "${AURAW_GPU_DETERMINISM_MAX_ABS:-0}" \
  --report "$REPORT_ROOT/gpu-determinism.json"

for backend in cpu gpu; do
  python3 "$ROOT/scripts/dev.py" regression compare \
    --manifest "$MANIFEST" --thresholds "$THRESHOLDS" \
    --reference-root "$REFERENCE_ROOT" \
    --candidate-root "$OUTPUT_ROOT/$backend/run-1" \
    --backend "$backend" --reference-engine "$REFERENCE_ENGINE" \
    --reference-engines "$REFERENCE_ENGINES" \
    --report-dir "$REPORT_ROOT/$backend-vs-$REFERENCE_ENGINE"
done

python3 "$ROOT/scripts/dev.py" regression cpu-gpu \
  --manifest "$MANIFEST" --thresholds "$THRESHOLDS" \
  --cpu-root "$OUTPUT_ROOT/cpu/run-1" \
  --gpu-root "$OUTPUT_ROOT/gpu/run-1" \
  --report-dir "$REPORT_ROOT/cpu-gpu"
'''

_SMOKE_REGRESSION_RENDERER_SH = r'''#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RENDERER="${AURAW_REGRESSION_RENDERER:-$ROOT/target/debug/auraw-regression-render}"
OUT="${AURAW_REGRESSION_SMOKE_DIR:-$ROOT/target/regression-smoke}"

mkdir -p "$OUT/run-1" "$OUT/run-2"
python3 "$ROOT/scripts/dev.py" regression validate-corpus \
  --manifest "$ROOT/regression/corpus.yaml" --verify-files
python3 "$ROOT/scripts/dev.py" regression validate-reference-engines \
  --config "$ROOT/regression/reference-engines.yaml"

for run in 1 2; do
  for scene in synthetic-bayer-multitarget synthetic-xtrans-multitarget; do
    "$RENDERER" --backend gpu \
      --input "$ROOT/regression/raw/${scene%%-multitarget}.dng" \
      --output "$OUT/run-$run/$scene.npz"
  done
done

python3 "$ROOT/scripts/dev.py" regression determinism \
  --manifest "$ROOT/regression/corpus.yaml" --backend gpu \
  --run-a "$OUT/run-1" --run-b "$OUT/run-2" \
  --max-abs 0 --report "$OUT/determinism.json"

python3 - "$OUT/run-1" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve()
project = root.parents[2]
sys.path.insert(0, str(project / "regression"))
from iqr.io import load_linear_image

for path in sorted(root.glob("*.npz")):
    image = load_linear_image(path, color_space="linear-rec2020-d65")
    if image.rgb.shape != (256, 256, 3):
        raise SystemExit(f"unexpected shape for {path}: {image.rgb.shape}")
    if image.metadata.get("renderer") != "auraw-regression-render":
        raise SystemExit(f"missing renderer metadata in {path}")
    print(f"validated {path.name}: {image.rgb.shape}, {image.rgb.dtype}")
PY
'''

_VERIFIED_DOWNLOAD_SH = r'''#!/usr/bin/env sh
set -eu

usage() {
    echo "usage: $0 URL OUTPUT EXPECTED_DIGEST_OR_HTTPS_SHA256_URL" >&2
    echo "digest formats: 64-hex SHA-256, sha256:HEX, or sha512:HEX" >&2
    exit 2
}

[ "$#" -eq 3 ] || usage
url=$1
output=$2
expected_source=$3

case "$url" in
    https://*) ;;
    *) echo "refusing non-HTTPS download: $url" >&2; exit 2 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

algorithm=
expected=
case "$expected_source" in
    https://*)
        checksum_text="$(
            curl --proto '=https' --tlsv1.2 --http1.1 \
                --fail --location --show-error \
                --retry 8 --retry-all-errors --retry-delay 3 \
                --connect-timeout 30 --max-time 300 \
                "$expected_source"
        )"
        algorithm=sha256
        expected="$(printf '%s\n' "$checksum_text" | grep -Eo '[0-9a-fA-F]{64}' | head -n 1 || true)"
        ;;
    sha256:*)
        algorithm=sha256
        expected=${expected_source#sha256:}
        ;;
    sha512:*)
        algorithm=sha512
        expected=${expected_source#sha512:}
        ;;
    *)
        algorithm=sha256
        expected=$expected_source
        ;;
esac

case "$expected" in
    ""|*[!0-9a-fA-F]*) echo "invalid checksum value" >&2; exit 2 ;;
esac

case "$algorithm" in
    sha256)
        [ "${#expected}" -eq 64 ] || {
            echo "SHA-256 must contain 64 hex digits" >&2
            exit 2
        }
        checksum_command=sha256sum
        ;;
    sha512)
        [ "${#expected}" -eq 128 ] || {
            echo "SHA-512 must contain 128 hex digits" >&2
            exit 2
        }
        checksum_command=sha512sum
        ;;
    *)
        echo "unsupported checksum algorithm: $algorithm" >&2
        exit 2
        ;;
esac

command -v "$checksum_command" >/dev/null 2>&1 || {
    echo "$checksum_command is required" >&2
    exit 1
}

# Normalize once so uppercase digests are accepted and diagnostics are stable.
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"

verify_file() {
    actual="$("$checksum_command" "$1" | awk '{print $1}')" || return 1
    if [ "$actual" != "$expected" ]; then
        echo "$algorithm checksum mismatch for $1" >&2
        echo "expected: $expected" >&2
        echo "actual:   $actual" >&2
        return 1
    fi
}

# A stale or corrupt cache entry should trigger a fresh download without making
# a successful recovery look like a failed build.
if [ -f "$output" ] && verify_file "$output" 2>/dev/null; then
    echo "verified cached download: $output"
    exit 0
fi

temporary="${output}.download.$$"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
rm -f "$temporary"
curl --proto '=https' --tlsv1.2 --http1.1 \
    --fail --location --show-error \
    --retry 8 --retry-all-errors --retry-delay 3 \
    --connect-timeout 30 --max-time 900 \
    "$url" -o "$temporary"
verify_file "$temporary"
chmod --reference="$output" "$temporary" 2>/dev/null || true
mv -f "$temporary" "$output"
trap - EXIT HUP INT TERM
'''

_VERIFY_ANDROID_16KB_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APK=${1:-"$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"}
BUILD_CONTRACT="$ROOT/android/build-contract.properties"
EXPECTED_NDK_VERSION=$(sed -n 's/^ndkVersion=//p' "$BUILD_CONTRACT")
BUILD_TOOLS_VERSION=$(sed -n 's/^buildToolsVersion=//p' "$BUILD_CONTRACT")

if [ "${1:-}" = "--print-build-contract" ]; then
    printf '{"ndkVersion":"%s","buildToolsVersion":"%s"}\n' "$EXPECTED_NDK_VERSION" "$BUILD_TOOLS_VERSION"
    exit 0
fi

if [ ! -f "$APK" ]; then
    echo "APK not found: $APK" >&2
    exit 1
fi

if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
if [ -z "$SDK" ]; then
    echo "Android SDK not found. Set ANDROID_SDK_ROOT (or ANDROID_HOME)." >&2
    exit 1
fi

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-"$SDK/ndk/$EXPECTED_NDK_VERSION"}}
if [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found at $NDK" >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi

NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
OBJDUMP="$NDK_HOST/bin/llvm-objdump"
ZIPALIGN="$SDK/build-tools/$BUILD_TOOLS_VERSION/zipalign"
if [ ! -x "$OBJDUMP" ]; then
    echo "llvm-objdump not found: $OBJDUMP" >&2
    exit 1
fi
if [ ! -x "$ZIPALIGN" ]; then
    echo "zipalign $BUILD_TOOLS_VERSION not found: $ZIPALIGN" >&2
    exit 1
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/auraw-16kb.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
unzip -qq "$APK" 'lib/*/*.so' -d "$TMP"

FOUND_64=0
for ABI in arm64-v8a x86_64; do
    LIBDIR="$TMP/lib/$ABI"
    [ -d "$LIBDIR" ] || continue
    for SO in "$LIBDIR"/*.so; do
        [ -f "$SO" ] || continue
        FOUND_64=1
        ALIGNMENTS=$(
            "$OBJDUMP" -p "$SO" \
                | sed -n '/LOAD/s/.*align 2\*\*\([0-9][0-9]*\).*/\1/p'
        )
        if [ -z "$ALIGNMENTS" ]; then
            echo "Could not read ELF LOAD alignment from $SO" >&2
            exit 1
        fi
        if printf '%s\n' "$ALIGNMENTS" | awk '$1 < 14 { bad = 1 } END { exit bad ? 0 : 1 }'; then
            echo "16 KB ELF alignment check failed: $SO" >&2
            "$OBJDUMP" -p "$SO" | grep LOAD >&2 || true
            exit 1
        fi
        echo "16 KB ELF aligned: ${SO#$TMP/}"
    done
done

if [ "$FOUND_64" -eq 0 ]; then
    echo "No 64-bit native libraries found; ELF 16 KB check not applicable."
fi

# -P 16 verifies that uncompressed native libraries are page-aligned in the APK.
"$ZIPALIGN" -c -P 16 -v 4 "$APK"
echo "Android 16 KB page-size checks passed: $APK"
'''

_VERIFY_SOURCE_REVISION_SH = r'''#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if ! command -v git >/dev/null 2>&1; then
    echo "git is required to verify the source revision" >&2
    exit 1
fi
if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "release builds must run from a Git checkout" >&2
    exit 1
fi

STATUS=$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)
if [ -n "$STATUS" ]; then
    echo "release builds require a clean source tree:" >&2
    printf '%s\n' "$STATUS" >&2
    exit 1
fi

git -C "$ROOT" rev-parse --verify HEAD
'''

SHELL_COMMANDS: dict[str, tuple[str, str]] = {
    'build-android-lensfun.sh': ('sh', _BUILD_ANDROID_LENSFUN_SH),
    'build-android-libraw.sh': ('sh', _BUILD_ANDROID_LIBRAW_SH),
    'build-android.sh': ('sh', _BUILD_ANDROID_SH),
    'build-linux.sh': ('sh', _BUILD_LINUX_SH),
    'run_image_regression.sh': ('bash', _RUN_IMAGE_REGRESSION_SH),
    'smoke-regression-renderer.sh': ('bash', _SMOKE_REGRESSION_RENDERER_SH),
    'verified-download.sh': ('sh', _VERIFIED_DOWNLOAD_SH),
    'verify-android-16kb.sh': ('sh', _VERIFY_ANDROID_16KB_SH),
    'verify-source-revision.sh': ('sh', _VERIFY_SOURCE_REVISION_SH),
}


def run_shell_command(name: str, arguments: Sequence[str]) -> int:
    """Run one embedded build/release script with dev.py as its original $0."""
    executable, source = SHELL_COMMANDS[name]
    command = [executable, "-c", source, str(ROOT / "scripts/dev.py"), *arguments]
    try:
        return subprocess.run(command, cwd=ROOT, check=False).returncode
    except OSError as error:
        print(f"error: unable to execute {executable}: {error}", file=sys.stderr)
        return 2


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
    regression_suite.set_defaults(handler=lambda _args: run_shell_command("run_image_regression.sh", ()))

    smoke = subparsers.add_parser("smoke-regression", help="run the deterministic regression-renderer smoke gate")
    smoke.set_defaults(handler=lambda _args: run_shell_command("smoke-regression-renderer.sh", ()))

    build_android = subparsers.add_parser("build-android", help="build Android native dependencies and AuRaw library")
    build_android.add_argument("abi", nargs="?", default="arm64-v8a")
    build_android.add_argument("profile", nargs="?", default="release")
    build_android.add_argument("--print-build-contract", action="store_true")
    build_android.set_defaults(handler=lambda args: run_shell_command(
        "build-android.sh", ["--print-build-contract"] if args.print_build_contract else [args.abi, args.profile]
    ))

    build_libraw = subparsers.add_parser("build-android-libraw", help="build pinned LibRaw for one Android ABI")
    build_libraw.add_argument("abi", nargs="?", default="arm64-v8a")
    build_libraw.add_argument("--print-build-contract", action="store_true")
    build_libraw.set_defaults(handler=lambda args: run_shell_command(
        "build-android-libraw.sh", ["--print-build-contract"] if args.print_build_contract else [args.abi]
    ))

    build_lensfun = subparsers.add_parser("build-android-lensfun", help="build pinned Lensfun for one Android ABI")
    build_lensfun.add_argument("abi", nargs="?", default="arm64-v8a")
    build_lensfun.set_defaults(handler=lambda args: run_shell_command("build-android-lensfun.sh", [args.abi]))

    build_linux = subparsers.add_parser("build-linux", help="build a revision-stamped Linux release")
    build_linux.set_defaults(handler=lambda _args: run_shell_command("build-linux.sh", ()))

    verify_android = subparsers.add_parser("verify-android-16kb", help="verify Android ELF and APK 16 KB alignment")
    verify_android.add_argument("apk", nargs="?", type=Path)
    verify_android.add_argument("--print-build-contract", action="store_true")
    verify_android.set_defaults(handler=lambda args: run_shell_command(
        "verify-android-16kb.sh",
        ["--print-build-contract"] if args.print_build_contract else ([str(args.apk)] if args.apk else []),
    ))

    download = subparsers.add_parser("verified-download", help="download an HTTPS artifact and verify its digest")
    download.add_argument("url")
    download.add_argument("output")
    download.add_argument("expected_digest")
    download.set_defaults(handler=lambda args: run_shell_command(
        "verified-download.sh", [args.url, args.output, args.expected_digest]
    ))

    revision = subparsers.add_parser("verify-source-revision", help="require a clean Git checkout and print HEAD")
    revision.set_defaults(handler=lambda _args: run_shell_command("verify-source-revision.sh", ()))

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


if __name__ == "__main__":
    raise SystemExit(main())
