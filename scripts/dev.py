#!/usr/bin/env python3
"""Development and CI validation commands for AuRaw."""

from __future__ import annotations

import argparse
from collections.abc import Callable, Sequence
from dataclasses import dataclass
import hashlib
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tomllib

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


def build_parser() -> argparse.ArgumentParser:
    """Build the command-line parser."""
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    check_all = subparsers.add_parser(
        "check-all",
        help="run source, workflow-pin, and Gradle-wrapper checks",
    )
    check_all.set_defaults(handler=lambda _args: run_checks(ALL_CHECKS))

    check_source = subparsers.add_parser(
        "check-source",
        help="check source reachability, shaders, and generated binaries",
    )
    check_source.set_defaults(handler=lambda _args: run_checks((CHECK_SOURCE,)))

    check_workflows = subparsers.add_parser(
        "check-workflows",
        help="validate immutable commit pins in CI workflows",
    )
    check_workflows.set_defaults(handler=lambda _args: run_checks((CHECK_WORKFLOWS,)))

    check_gradle = subparsers.add_parser(
        "check-gradle",
        help="validate the checked-in Gradle wrapper",
    )
    check_gradle.set_defaults(handler=lambda _args: run_checks((CHECK_GRADLE,)))

    validate_math = subparsers.add_parser(
        "validate-math",
        help="run analytical camera-profile and demosaic tests",
    )
    validate_math.add_argument(
        "--release",
        action="store_true",
        help="run the selected Rust tests in release mode",
    )
    validate_math.set_defaults(handler=command_validate_math)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Parse arguments and dispatch a development command."""
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.handler(args))


if __name__ == "__main__":
    raise SystemExit(main())
