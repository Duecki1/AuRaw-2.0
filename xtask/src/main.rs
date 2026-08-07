use image::{imageops::FilterType, DynamicImage, ImageFormat, Rgba, RgbaImage};
use ring::digest::{Context as DigestContext, SHA256, SHA512};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use zip::ZipArchive;


const BENCHMARK_SCENES: [(&str, &str, u32, u32); 2] = [
    (
        "synthetic-bayer-multitarget",
        "synthetic-bayer.dng",
        256,
        256,
    ),
    (
        "synthetic-xtrans-multitarget",
        "synthetic-xtrans.dng",
        256,
        256,
    ),
];


const CAMERA_PROFILE_TEST_FILTERS: &[&str] = &[
    "pipeline::color_profile::tests",
    "pipeline::color_profile::dcp::tests",
    "pipeline::color_profile::icc::tests",
    "pipeline::sigmoid::tests",
    "gpu_params_follow_the_wgsl_uniform_layout",
    "profile_shader_parses_with_the_profile_storage_contract",
    "adjustments_shader_exposes_darktable_sigmoid_paths",
    "scene_graph_preserves_native_call_order_and_stage_ownership",
    "global_wb_changes_raw_multipliers_without_changing_the_camera_transform",
];

const DEMOSAIC_TEST_FILTERS: &[&str] = &[
    "compute_shaders_parse_and_validate",
    "demosaic_contracts_are_compiler_validated",
    "demosaic_shaders_expose_every_dispatched_entry_point",
    "inpaint_opposed",
];

const MATH_TEST_GROUPS: [(&str, &[&str]); 2] = [
    ("camera profile", CAMERA_PROFILE_TEST_FILTERS),
    ("demosaic", DEMOSAIC_TEST_FILTERS),
];

const ANDROID_64_BIT_ABIS: [&str; 2] = ["arm64-v8a", "x86_64"];

const REQUIRED_WORKSPACE_METADATA: [(&str, MetadataKind); 8] = [
    ("android_ndk_version", MetadataKind::String),
    ("android_build_tools_version", MetadataKind::String),
    ("android_compile_sdk", MetadataKind::PositiveInteger),
    ("android_min_sdk", MetadataKind::PositiveInteger),
    ("android_target_sdk", MetadataKind::PositiveInteger),
    ("libraw_revision", MetadataKind::String),
    ("lensfun_revision", MetadataKind::String),
    ("android_use_legacy_packaging", MetadataKind::Boolean),
];

const EXPECTED_GRADLE_VERSION: &str = "8.11.1";
const EXPECTED_GRADLE_DISTRIBUTION_SHA256: &str =
    "f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6";
const EXPECTED_GRADLE_WRAPPER_JAR_SHA256: &str =
    "2db75c40782f5e8ba1fc278a5574bab070adccb2d21ca5a6e5ed840888448046";

const BINARY_SUFFIXES: [&str; 13] = [
    "a", "aar", "apk", "class", "dll", "dylib", "exe", "jar", "o", "obj", "rlib",
    "rmeta", "so",
];
const ALLOWED_BINARY_PATHS: [&str; 1] = ["gradle/wrapper/gradle-wrapper.jar"];
const IGNORED_BINARY_ROOTS: [&str; 9] = [
    ".git",
    ".gradle",
    "dist",
    "target",
    "android/.gradle",
    "android/build",
    // Android Gradle Plugin CMake staging output. It is generated locally and
    // restored by the Gitea native-build cache before source validation runs.
    "android/app/.cxx",
    "android/app/build",
    "android/native",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.message.is_empty() {
                eprintln!("error: {error}");
            }
            ExitCode::from(error.code.clamp(1, 255) as u8)
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os();
    let _program = args.next();
    let Some(command) = args.next() else {
        print_help();
        return Err(XtaskError::usage("missing command"));
    };
    let rest: Vec<OsString> = args.collect();

    match command.to_string_lossy().as_ref() {
        "check-all" => {
            ensure_no_extra_args(&rest, "check-all")?;
            command_checks(&[CheckKind::Source, CheckKind::Workflows, CheckKind::Gradle])
        }
        "check-source" => {
            ensure_no_extra_args(&rest, "check-source")?;
            command_checks(&[CheckKind::Source])
        }
        "check-workflows" => {
            ensure_no_extra_args(&rest, "check-workflows")?;
            command_checks(&[CheckKind::Workflows])
        }
        "check-gradle" => {
            ensure_no_extra_args(&rest, "check-gradle")?;
            command_checks(&[CheckKind::Gradle])
        }
        "validate-math" => command_validate_math(parse_validate_math_args(rest)?),
        "print-metadata" | "print-build-metadata" => {
            command_print_metadata(parse_print_metadata_args(rest)?)
        }
        "verified-download" => command_verified_download(parse_verified_download_args(rest)?),
        "verify-source-revision" => {
            ensure_no_extra_args(&rest, "verify-source-revision")?;
            command_verify_source_revision()
        }
        "bench" => command_bench(parse_bench_args(rest)?),
        "icons" => {
            ensure_no_extra_args(&rest, "icons")?;
            command_icons()
        }
        "build-android" => command_build_android(parse_build_android_args(rest)?),
        "build-android-libraw" => {
            command_build_android_dependency(parse_build_dependency_args(rest, "build-android-libraw")?, AndroidDependency::LibRaw)
        }
        "build-android-lensfun" => {
            command_build_android_dependency(parse_build_dependency_args(rest, "build-android-lensfun")?, AndroidDependency::Lensfun)
        }
        "build-linux" => {
            ensure_no_extra_args(&rest, "build-linux")?;
            command_build_linux()
        }
        "verify-android-16kb" => command_verify_android_16kb(parse_android_args(rest)?),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            Err(XtaskError::usage(format!("unknown command: {other}")))
        }
    }
}

fn print_help() {
    println!(
        "AuRaw development and CI validation commands.

         Usage: cargo xtask <command> [options]

         Commands:
           check-all
           check-source
           check-workflows
           check-gradle
           validate-math [--release]
           bench [--renderer PATH] [--runs N] [--output PATH]
                 [--budget-file PATH] [--enforce-budget] [--dry-run]
           icons
           build-android [ABI] [PROFILE] [--print-build-contract]
           build-android-libraw [ABI] [--print-build-contract]
           build-android-lensfun [ABI] [--print-build-contract]
           build-linux
           print-metadata [--format json|shell] [--value FIELD]
           verified-download <URL> <OUTPUT> <EXPECTED-DIGEST>
           verify-source-revision
           verify-android-16kb [APK] [--print-build-contract]
                               [--objdump PATH] [--zipalign PATH]"
    );
}

#[derive(Debug)]
struct XtaskError {
    message: String,
    code: i32,
}

impl XtaskError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 2,
        }
    }

    fn with_code(message: impl Into<String>, code: i32) -> Self {
        Self {
            message: message.into(),
            code: if code == 0 { 1 } else { code },
        }
    }

    fn silent(code: i32) -> Self {
        Self {
            message: String::new(),
            code: if code == 0 { 1 } else { code },
        }
    }
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for XtaskError {}

impl From<io::Error> for XtaskError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for XtaskError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<zip::result::ZipError> for XtaskError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::new(error.to_string())
    }
}

type Result<T> = std::result::Result<T, XtaskError>;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be located directly under the workspace root")
        .to_path_buf()
}

fn rooted(root: &Path, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn ensure_no_extra_args(args: &[OsString], command: &str) -> Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(XtaskError::usage(format!(
            "{command} does not accept arguments: {}",
            args.iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        )))
    }
}

fn next_value(args: &[OsString], index: &mut usize, option: &str) -> Result<OsString> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| XtaskError::usage(format!("{option} requires a value")))
}

#[derive(Debug, Clone, Copy)]
enum MetadataFormat {
    Json,
    Shell,
}

#[derive(Debug)]
struct MetadataArgs {
    format: MetadataFormat,
    value: Option<String>,
}

const BUILD_METADATA_FIELDS: [&str; 8] = [
    "ndkVersion",
    "buildToolsVersion",
    "compileSdk",
    "minSdk",
    "targetSdk",
    "librawRevision",
    "lensfunRevision",
    "useLegacyPackaging",
];

fn parse_print_metadata_args(args: Vec<OsString>) -> Result<MetadataArgs> {
    let mut parsed = MetadataArgs {
        format: MetadataFormat::Json,
        value: None,
    };
    let mut index = 0usize;
    while index < args.len() {
        let argument = args[index].to_string_lossy();
        if argument == "--format" {
            let value = next_value(&args, &mut index, "--format")?;
            parsed.format = parse_metadata_format(&value.to_string_lossy())?;
        } else if let Some(value) = argument.strip_prefix("--format=") {
            parsed.format = parse_metadata_format(value)?;
        } else if argument == "--value" {
            let value = next_value(&args, &mut index, "--value")?
                .to_string_lossy()
                .into_owned();
            validate_metadata_field(&value)?;
            parsed.value = Some(value);
        } else if let Some(value) = argument.strip_prefix("--value=") {
            validate_metadata_field(value)?;
            parsed.value = Some(value.to_owned());
        } else if matches!(argument.as_ref(), "--help" | "-h") {
            print_help();
            std::process::exit(0);
        } else {
            return Err(XtaskError::usage(format!(
                "unknown print-metadata option: {argument}"
            )));
        }
        index += 1;
    }
    Ok(parsed)
}

fn parse_metadata_format(value: &str) -> Result<MetadataFormat> {
    match value {
        "json" => Ok(MetadataFormat::Json),
        "shell" => Ok(MetadataFormat::Shell),
        _ => Err(XtaskError::usage("--format must be either json or shell")),
    }
}

fn validate_metadata_field(value: &str) -> Result<()> {
    if BUILD_METADATA_FIELDS.contains(&value) {
        Ok(())
    } else {
        Err(XtaskError::usage(format!(
            "--value must be one of: {}",
            BUILD_METADATA_FIELDS.join(", ")
        )))
    }
}

#[derive(Debug)]
struct VerifiedDownloadArgs {
    url: String,
    output: PathBuf,
    expected_digest: String,
}

fn parse_verified_download_args(args: Vec<OsString>) -> Result<VerifiedDownloadArgs> {
    if args.len() != 3 {
        return Err(XtaskError::usage(
            "verified-download requires <url> <output> <expected-digest>",
        ));
    }
    let url = args[0]
        .to_str()
        .ok_or_else(|| XtaskError::usage("download URL must be valid UTF-8"))?
        .to_owned();
    let expected_digest = args[2]
        .to_str()
        .ok_or_else(|| XtaskError::usage("expected digest must be valid UTF-8"))?
        .to_owned();
    Ok(VerifiedDownloadArgs {
        url,
        output: PathBuf::from(args[1].clone()),
        expected_digest,
    })
}

#[derive(Debug)]
struct BenchArgs {
    renderer: PathBuf,
    runs: i64,
    budget_file: PathBuf,
    output: PathBuf,
    enforce_budget: bool,
    dry_run: bool,
}

fn parse_bench_args(args: Vec<OsString>) -> Result<BenchArgs> {
    let mut parsed = BenchArgs {
        renderer: PathBuf::from("target/release/auraw-regression-render"),
        runs: 3,
        budget_file: PathBuf::from("benchmarks/gpu-budget.json"),
        output: PathBuf::from("target/benchmark-report.json"),
        enforce_budget: false,
        dry_run: false,
    };

    let mut index = 0usize;
    while index < args.len() {
        let argument = args[index].to_string_lossy();
        if argument == "--renderer" {
            parsed.renderer = PathBuf::from(next_value(&args, &mut index, "--renderer")?);
        } else if let Some(value) = argument.strip_prefix("--renderer=") {
            parsed.renderer = PathBuf::from(value);
        } else if argument == "--runs" {
            let value = next_value(&args, &mut index, "--runs")?;
            parsed.runs = parse_i64(&value, "--runs")?;
        } else if let Some(value) = argument.strip_prefix("--runs=") {
            parsed.runs = value
                .parse::<i64>()
                .map_err(|_| XtaskError::usage("--runs must be an integer"))?;
        } else if argument == "--budget-file" {
            parsed.budget_file = PathBuf::from(next_value(&args, &mut index, "--budget-file")?);
        } else if let Some(value) = argument.strip_prefix("--budget-file=") {
            parsed.budget_file = PathBuf::from(value);
        } else if argument == "--output" {
            parsed.output = PathBuf::from(next_value(&args, &mut index, "--output")?);
        } else if let Some(value) = argument.strip_prefix("--output=") {
            parsed.output = PathBuf::from(value);
        } else if argument == "--enforce-budget" {
            parsed.enforce_budget = true;
        } else if argument == "--dry-run" {
            parsed.dry_run = true;
        } else if matches!(argument.as_ref(), "--help" | "-h") {
            print_help();
            std::process::exit(0);
        } else {
            return Err(XtaskError::usage(format!("unknown bench option: {argument}")));
        }
        index += 1;
    }
    Ok(parsed)
}

fn parse_i64(value: &OsStr, option: &str) -> Result<i64> {
    value
        .to_string_lossy()
        .parse::<i64>()
        .map_err(|_| XtaskError::usage(format!("{option} must be an integer")))
}

#[derive(Debug, Clone, Copy)]
struct ValidateMathArgs {
    release: bool,
}

fn parse_validate_math_args(args: Vec<OsString>) -> Result<ValidateMathArgs> {
    let mut release = false;
    for argument in args {
        match argument.to_string_lossy().as_ref() {
            "--release" => release = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            unknown => {
                return Err(XtaskError::usage(format!(
                    "unknown validate-math option: {unknown}"
                )))
            }
        }
    }
    Ok(ValidateMathArgs { release })
}

#[derive(Debug)]
struct BuildAndroidArgs {
    abi: String,
    profile: String,
    print_build_contract: bool,
}

fn parse_build_android_args(args: Vec<OsString>) -> Result<BuildAndroidArgs> {
    let mut positionals = Vec::new();
    let mut print_build_contract = false;
    for argument in args {
        match argument.to_string_lossy().as_ref() {
            "--print-build-contract" => print_build_contract = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(XtaskError::usage(format!(
                    "unknown build-android option: {value}"
                )))
            }
            _ => positionals.push(argument.to_string_lossy().into_owned()),
        }
    }
    if positionals.len() > 2 {
        return Err(XtaskError::usage(
            "build-android accepts at most ABI and PROFILE positional arguments",
        ));
    }
    Ok(BuildAndroidArgs {
        abi: positionals.first().cloned().unwrap_or_else(|| "arm64-v8a".to_owned()),
        profile: positionals.get(1).cloned().unwrap_or_else(|| "release".to_owned()),
        print_build_contract,
    })
}

#[derive(Debug)]
struct BuildDependencyArgs {
    abi: String,
    print_build_contract: bool,
}

fn parse_build_dependency_args(args: Vec<OsString>, command: &str) -> Result<BuildDependencyArgs> {
    let mut abi = None;
    let mut print_build_contract = false;
    for argument in args {
        match argument.to_string_lossy().as_ref() {
            "--print-build-contract" => print_build_contract = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(XtaskError::usage(format!("unknown {command} option: {value}")))
            }
            _ if abi.is_none() => abi = Some(argument.to_string_lossy().into_owned()),
            _ => {
                return Err(XtaskError::usage(format!(
                    "{command} accepts at most one ABI positional argument"
                )))
            }
        }
    }
    Ok(BuildDependencyArgs {
        abi: abi.unwrap_or_else(|| "arm64-v8a".to_owned()),
        print_build_contract,
    })
}

#[derive(Debug)]
struct AndroidArgs {
    apk: PathBuf,
    print_build_contract: bool,
    objdump: Option<PathBuf>,
    zipalign: Option<PathBuf>,
}

fn parse_android_args(args: Vec<OsString>) -> Result<AndroidArgs> {
    let mut apk = None;
    let mut print_build_contract = false;
    let mut objdump = None;
    let mut zipalign = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].to_string_lossy().as_ref() {
            "--print-build-contract" => print_build_contract = true,
            "--objdump" => {
                objdump = Some(PathBuf::from(next_value(&args, &mut index, "--objdump")?));
            }
            "--zipalign" => {
                zipalign = Some(PathBuf::from(next_value(&args, &mut index, "--zipalign")?));
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(XtaskError::usage(format!(
                    "unknown verify-android-16kb option: {value}"
                )))
            }
            _ => {
                if apk.is_some() {
                    return Err(XtaskError::usage(
                        "verify-android-16kb accepts exactly one APK path",
                    ));
                }
                apk = Some(PathBuf::from(args[index].clone()));
            }
        }
        index += 1;
    }

    Ok(AndroidArgs {
        apk: apk.unwrap_or_else(|| {
            PathBuf::from("android/app/build/outputs/apk/debug/app-debug.apk")
        }),
        print_build_contract,
        objdump,
        zipalign,
    })
}

#[derive(Debug, Clone, Copy)]
enum CheckKind {
    Source,
    Workflows,
    Gradle,
}

fn command_checks(checks: &[CheckKind]) -> Result<()> {
    let root = workspace_root();
    let mut failed = 0usize;
    for check in checks {
        let (title, success_message, result) = match check {
            CheckKind::Source => {
                let result = (|| {
                    let mut errors = validate_source_reachability(&root)?;
                    errors.extend(validate_shader_imports(&root)?);
                    errors.extend(validate_generated_binaries(&root)?);
                    errors.sort();
                    errors.dedup();
                    Ok(errors)
                })();
                (
                    "Source tree",
                    "connected Rust modules, tracked shaders, and source-tree binaries verified",
                    result,
                )
            }
            CheckKind::Workflows => (
                "Workflow pins",
                "all third-party workflow actions are pinned to full commit SHAs",
                validate_workflow_pins(&root),
            ),
            CheckKind::Gradle => (
                "Gradle wrapper",
                "Gradle wrapper 8.11.1 integrity verified (2db75c40782f5e8ba1fc278a5574bab070adccb2d21ca5a6e5ed840888448046)",
                validate_gradle_wrapper(&root),
            ),
        };

        println!("== {title} ==");
        let errors = match result {
            Ok(errors) => errors,
            Err(error) => vec![format!("unexpected XtaskError: {error}")],
        };
        if errors.is_empty() {
            println!("PASS: {success_message}");
        } else {
            failed += 1;
            eprintln!("FAIL: {} issue(s)", errors.len());
            for error in errors {
                eprintln!("  - {error}");
            }
        }
        println!();
    }

    if checks.len() > 1 {
        println!(
            "Validation summary: {} passed, {failed} failed",
            checks.len() - failed
        );
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(XtaskError::silent(1))
    }
}

fn command_validate_math(args: ValidateMathArgs) -> Result<()> {
    if find_executable("cargo").is_none() {
        return Err(XtaskError::usage(
            "cargo is required because math validation compiles Rust and validates WGSL with Naga",
        ));
    }
    let root = workspace_root();
    let mut failed = Vec::new();
    let total: usize = MATH_TEST_GROUPS.iter().map(|(_, filters)| filters.len()).sum();

    for (group_name, filters) in MATH_TEST_GROUPS {
        let title = group_name
            .split_whitespace()
            .map(|word| {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!("== {title} validation ({} test filters) ==", filters.len());
        for filter in filters {
            let filter = *filter;
            let mut display = vec!["cargo", "test", "--locked", "--lib"];
            if args.release {
                display.push("--release");
            }
            display.extend([filter, "--", "--nocapture"]);
            println!("  $ {}", display.join(" "));

            let mut command = Command::new("cargo");
            command.args(["test", "--locked", "--lib"]);
            if args.release {
                command.arg("--release");
            }
            let status = command
                .args([filter, "--", "--nocapture"])
                .current_dir(&root)
                .status();
            match status {
                Ok(status) if status.success() => {}
                Ok(_) => failed.push(filter.to_owned()),
                Err(error) => {
                    eprintln!("  unable to execute cargo: {error}");
                    failed.push(filter.to_owned());
                }
            }
        }
        println!();
    }

    if !failed.is_empty() {
        eprintln!(
            "Math validation failed for {} of {total} test filters:",
            failed.len()
        );
        for filter in failed {
            eprintln!("  - {filter}");
        }
        return Err(XtaskError::silent(1));
    }

    let mode = if args.release { "release" } else { "debug" };
    println!("PASS: all {total} compiler-backed math test filters passed ({mode} mode)");
    Ok(())
}

fn validate_generated_binaries(root: &Path) -> Result<Vec<String>> {
    fn visit(root: &Path, directory: &Path, errors: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(directory).map_err(|error| {
            XtaskError::new(format!("cannot read directory {}: {error}", directory.display()))
        })? {
            let entry = entry?;
            let path = entry.path();
            let relative = relative_display(root, &path);
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if IGNORED_BINARY_ROOTS.iter().any(|ignored| {
                    relative == *ignored || relative.starts_with(&format!("{ignored}/"))
                }) {
                    continue;
                }
                visit(root, &path, errors)?;
            } else if file_type.is_file() {
                let extension = path
                    .extension()
                    .and_then(OsStr::to_str)
                    .map(str::to_ascii_lowercase);
                if extension
                    .as_deref()
                    .is_some_and(|extension| BINARY_SUFFIXES.contains(&extension))
                    && !ALLOWED_BINARY_PATHS.contains(&relative.as_str())
                {
                    errors.push(format!(
                        "generated binary is present in the source tree: {relative}"
                    ));
                }
            }
        }
        Ok(())
    }

    let mut errors = Vec::new();
    visit(root, root, &mut errors)?;
    errors.sort();
    Ok(errors)
}

fn validate_workflow_pins(root: &Path) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for workflow_root in [root.join(".github/workflows"), root.join(".gitea/workflows")] {
        if !workflow_root.is_dir() {
            continue;
        }
        let mut workflows = Vec::new();
        collect_files_with_extensions(&workflow_root, &["yml", "yaml"], &mut workflows)?;
        workflows.sort();
        for path in workflows {
            let source = fs::read_to_string(&path).map_err(|error| {
                XtaskError::new(format!(
                    "cannot read workflow {}: {error}",
                    relative_display(root, &path)
                ))
            })?;
            for (line_index, line) in source.lines().enumerate() {
                let Some(action) = workflow_action_reference(line) else {
                    continue;
                };
                if action.starts_with("./") || action.starts_with("docker://") {
                    continue;
                }
                let revision = action.rsplit_once('@').map(|(_, revision)| revision);
                if !revision.is_some_and(is_full_commit_sha) {
                    errors.push(format!(
                        "{}:{}: mutable action reference {action}",
                        relative_display(root, &path),
                        line_index + 1
                    ));
                }
            }
        }
    }
    errors.sort();
    Ok(errors)
}

fn workflow_action_reference(line: &str) -> Option<String> {
    let line = strip_yaml_comment(line);
    let bytes = line.as_bytes();
    let mut search_from = 0usize;
    while let Some(relative) = line[search_from..].find("uses:") {
        let index = search_from + relative;
        let boundary_ok = index == 0
            || bytes[index - 1].is_ascii_whitespace()
            || matches!(bytes[index - 1], b'-' | b'{' | b',');
        if boundary_ok {
            let tail = line[index + "uses:".len()..].trim_start();
            let token = tail
                .split(|character: char| character.is_whitespace() || character == '#')
                .next()
                .unwrap_or_default()
                .trim_end_matches(|character| matches!(character, ',' | '}'))
                .trim_matches(|character| matches!(character, '\'' | '"'));
            if !token.is_empty() {
                return Some(token.to_owned());
            }
        }
        search_from = index + "uses:".len();
    }
    None
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        match quote {
            Some('"') => {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quote = None;
                }
            }
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                }
            }
            _ if character == '"' || character == '\'' => quote = Some(character),
            _ if character == '#' => return &line[..index],
            _ => {}
        }
    }
    line
}

fn is_full_commit_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_gradle_wrapper(root: &Path) -> Result<Vec<String>> {
    let properties_path = root.join("gradle/wrapper/gradle-wrapper.properties");
    let jar_path = root.join("gradle/wrapper/gradle-wrapper.jar");
    let gradlew = root.join("gradlew");
    let gradlew_bat = root.join("gradlew.bat");
    let required = [&properties_path, &jar_path, &gradlew, &gradlew_bat];
    let mut errors = Vec::new();
    for path in required {
        if !path.is_file() {
            errors.push(format!("missing wrapper file: {}", relative_display(root, path)));
        }
    }
    if !errors.is_empty() {
        return Ok(errors);
    }

    let properties = match parse_properties_file(&properties_path) {
        Ok(properties) => properties,
        Err(error) => return Ok(vec![error.to_string()]),
    };
    let distribution_url = properties
        .get("distributionUrl")
        .map(|value| value.replace("\\:", ":"))
        .unwrap_or_default();
    let expected_suffix = format!("/gradle-{EXPECTED_GRADLE_VERSION}-bin.zip");
    if !distribution_url.starts_with("https://services.gradle.org/distributions/") {
        errors.push(
            "distributionUrl must use the official HTTPS Gradle distribution host".to_owned(),
        );
    }
    if !distribution_url.ends_with(&expected_suffix) {
        errors.push(format!(
            "distributionUrl must select Gradle {EXPECTED_GRADLE_VERSION}; found {}",
            if distribution_url.is_empty() {
                "<missing>"
            } else {
                &distribution_url
            }
        ));
    }
    if properties.get("distributionSha256Sum").map(String::as_str)
        != Some(EXPECTED_GRADLE_DISTRIBUTION_SHA256)
    {
        errors.push("distributionSha256Sum does not match the pinned Gradle distribution".to_owned());
    }
    if properties
        .get("validateDistributionUrl")
        .map(|value| value.eq_ignore_ascii_case("true"))
        != Some(true)
    {
        errors.push("validateDistributionUrl must remain enabled".to_owned());
    }

    match sha256_file(&jar_path) {
        Ok(actual) if actual != EXPECTED_GRADLE_WRAPPER_JAR_SHA256 => errors.push(format!(
            "gradle-wrapper.jar checksum mismatch: expected {EXPECTED_GRADLE_WRAPPER_JAR_SHA256}, found {actual}"
        )),
        Ok(_) => {}
        Err(error) => errors.push(format!(
            "cannot hash {}: {error}",
            relative_display(root, &jar_path)
        )),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(&gradlew) {
            Ok(metadata) if metadata.permissions().mode() & 0o100 == 0 => {
                errors.push("gradlew must be executable".to_owned())
            }
            Ok(_) => {}
            Err(error) => errors.push(format!("cannot inspect gradlew permissions: {error}")),
        }
    }

    match (fs::read_to_string(&gradlew), fs::read_to_string(&gradlew_bat)) {
        (Ok(shell), Ok(batch)) => {
            if !shell
                .replace("$APP_HOME/", "")
                .contains("gradle/wrapper/gradle-wrapper.jar")
            {
                errors.push("gradlew does not reference the checked-in wrapper JAR".to_owned());
            }
            let normalized_batch = batch.replace('\\', "/").to_ascii_lowercase();
            if !normalized_batch.contains("gradle/wrapper/gradle-wrapper.jar") {
                errors.push("gradlew.bat does not reference the checked-in wrapper JAR".to_owned());
            }
        }
        (shell, batch) => {
            let details = [shell.err(), batch.err()]
                .into_iter()
                .flatten()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            errors.push(format!("cannot inspect Gradle launcher scripts: {details}"));
        }
    }

    errors.sort();
    Ok(errors)
}

fn parse_properties_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let source = fs::read_to_string(path)
        .map_err(|error| XtaskError::new(format!("cannot read {}: {error}", path.display())))?;
    let mut values = BTreeMap::new();
    for (line_index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(XtaskError::new(format!(
                "{}:{}: expected key=value",
                path.display(),
                line_index + 1
            )));
        };
        values.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(values)
}

fn collect_files_with_extensions(
    directory: &Path,
    extensions: &[&str],
    output: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|error| {
        XtaskError::new(format!("cannot read directory {}: {error}", directory.display()))
    })? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_with_extensions(&path, extensions, output)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extensions.contains(&extension))
        {
            output.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    String(String),
    Punct(char),
}

fn lex_rust(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }

        if bytes.get(index..index + 2) == Some(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }

        if bytes.get(index..index + 2) == Some(b"/*") {
            index += 2;
            let mut depth = 1usize;
            while index < bytes.len() && depth > 0 {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            continue;
        }

        if let Some((value, next)) = parse_raw_string(bytes, index) {
            tokens.push(Token::String(value));
            index = next;
            continue;
        }

        let quote_index = if byte == b'"' {
            Some(index)
        } else if byte == b'b' && bytes.get(index + 1) == Some(&b'"') {
            Some(index + 1)
        } else {
            None
        };
        if let Some(quote_index) = quote_index {
            let (value, next) = parse_quoted_string(bytes, quote_index);
            tokens.push(Token::String(value));
            index = next;
            continue;
        }

        if byte == b'\'' {
            if let Some(next) = skip_char_literal(bytes, index) {
                index = next;
            } else {
                index += 1;
            }
            continue;
        }

        if byte == b'_' || byte.is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
            tokens.push(Token::Ident(
                String::from_utf8_lossy(&bytes[start..index]).into_owned(),
            ));
            continue;
        }

        tokens.push(Token::Punct(byte as char));
        index += 1;
    }

    tokens
}

fn parse_raw_string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let r_index = if bytes.get(start) == Some(&b'r') {
        start
    } else if bytes.get(start) == Some(&b'b') && bytes.get(start + 1) == Some(&b'r') {
        start + 1
    } else {
        return None;
    };

    let mut cursor = r_index + 1;
    let mut hashes = 0usize;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }

    let content_start = cursor + 1;
    cursor = content_start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes
                .get(cursor + 1..cursor + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            let value = String::from_utf8_lossy(&bytes[content_start..cursor]).into_owned();
            return Some((value, cursor + 1 + hashes));
        }
        cursor += 1;
    }

    Some((
        String::from_utf8_lossy(&bytes[content_start..]).into_owned(),
        bytes.len(),
    ))
}

fn parse_quoted_string(bytes: &[u8], quote_index: usize) -> (String, usize) {
    let mut value = String::new();
    let mut index = quote_index + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return (value, index + 1),
            b'\\' if index + 1 < bytes.len() => {
                let escaped = bytes[index + 1];
                match escaped {
                    b'\\' => value.push('\\'),
                    b'"' => value.push('"'),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    other => value.push(other as char),
                }
                index += 2;
            }
            byte => {
                value.push(byte as char);
                index += 1;
            }
        }
    }
    (value, bytes.len())
}

fn skip_char_literal(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    if index >= bytes.len() {
        return None;
    }
    if bytes[index] == b'\\' {
        index += 2;
    } else {
        index += 1;
    }
    if bytes.get(index) == Some(&b'\'') {
        Some(index + 1)
    } else {
        None
    }
}

fn token_is_ident(token: Option<&Token>, expected: &str) -> bool {
    matches!(token, Some(Token::Ident(value)) if value == expected)
}

fn token_is_punct(token: Option<&Token>, expected: char) -> bool {
    matches!(token, Some(Token::Punct(value)) if *value == expected)
}

#[derive(Debug, Default)]
struct RustReferences {
    modules: Vec<(String, Option<String>)>,
    includes: Vec<String>,
}

fn parse_rust_references(source: &str) -> RustReferences {
    let tokens = lex_rust(source);
    let mut references = RustReferences::default();
    let mut pending_path = None;
    let mut index = 0usize;

    while index < tokens.len() {
        if token_is_punct(tokens.get(index), '#')
            && token_is_punct(tokens.get(index + 1), '[')
        {
            let mut end = index + 2;
            let mut bracket_depth = 1usize;
            while end < tokens.len() && bracket_depth > 0 {
                if token_is_punct(tokens.get(end), '[') {
                    bracket_depth += 1;
                } else if token_is_punct(tokens.get(end), ']') {
                    bracket_depth -= 1;
                }
                end += 1;
            }
            let attribute = &tokens[index + 2..end.saturating_sub(1)];
            if token_is_ident(attribute.first(), "path")
                && token_is_punct(attribute.get(1), '=')
            {
                if let Some(Token::String(path)) = attribute.get(2) {
                    pending_path = Some(path.clone());
                }
            }
            index = end;
            continue;
        }

        if token_is_ident(tokens.get(index), "mod") {
            if let Some(Token::Ident(name)) = tokens.get(index + 1) {
                if token_is_punct(tokens.get(index + 2), ';') {
                    references.modules.push((name.clone(), pending_path.take()));
                    index += 3;
                    continue;
                }
                pending_path = None;
            }
        }

        if token_is_ident(tokens.get(index), "include")
            && token_is_punct(tokens.get(index + 1), '!')
            && token_is_punct(tokens.get(index + 2), '(')
        {
            if let Some(Token::String(path)) = tokens.get(index + 3) {
                references.includes.push(path.clone());
            }
        }

        if matches!(
            tokens.get(index),
            Some(Token::Ident(keyword))
                if matches!(
                    keyword.as_str(),
                    "fn" | "struct" | "enum" | "union" | "trait" | "impl" | "type"
                        | "const" | "static" | "use" | "extern" | "macro_rules"
                )
        ) {
            pending_path = None;
        }

        index += 1;
    }

    references
}

#[derive(Debug)]
struct MetadataSources {
    crate_roots: Vec<PathBuf>,
    source_directories: Vec<PathBuf>,
}

fn cargo_metadata_sources(root: &Path) -> Result<MetadataSources> {
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|error| XtaskError::new(format!("could not execute cargo metadata: {error}")))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(XtaskError::with_code(
            if stderr.is_empty() {
                "cargo metadata failed".to_owned()
            } else {
                format!("cargo metadata failed: {stderr}")
            },
            code,
        ));
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| XtaskError::new("cargo metadata did not contain packages"))?;

    let mut crate_roots = BTreeSet::new();
    let mut source_directories = BTreeSet::new();
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(Value::as_str)
            .ok_or_else(|| XtaskError::new("cargo metadata package has no manifest_path"))?;
        let package_root = Path::new(manifest)
            .parent()
            .ok_or_else(|| XtaskError::new(format!("invalid manifest path: {manifest}")))?;
        let source_directory = package_root.join("src");
        if source_directory.is_dir() {
            source_directories.insert(source_directory);
        }

        let targets = package
            .get("targets")
            .and_then(Value::as_array)
            .ok_or_else(|| XtaskError::new(format!("package {manifest} has no targets")))?;
        for target in targets {
            if let Some(source) = target.get("src_path").and_then(Value::as_str) {
                crate_roots.insert(PathBuf::from(source));
            }
        }
    }

    Ok(MetadataSources {
        crate_roots: crate_roots.into_iter().collect(),
        source_directories: source_directories.into_iter().collect(),
    })
}

fn validate_source_reachability(root: &Path) -> Result<Vec<String>> {
    let metadata = cargo_metadata_sources(root)?;
    let mut validator = ModuleValidator {
        root,
        visited: HashSet::new(),
        errors: Vec::new(),
    };

    for crate_root in metadata.crate_roots {
        let module_directory = crate_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        validator.visit(&crate_root, &module_directory);
    }

    for source_directory in metadata.source_directories {
        let mut rust_files = Vec::new();
        collect_files_with_extension(&source_directory, "rs", &mut rust_files)?;
        for source in rust_files {
            let key = canonical_or_owned(&source);
            if !validator.visited.contains(&key) {
                validator.errors.push(format!(
                    "stale Rust source is not reachable from a Cargo target: {}",
                    relative_display(root, &source)
                ));
            }
        }
    }

    validator.errors.sort();
    validator.errors.dedup();
    Ok(validator.errors)
}

struct ModuleValidator<'a> {
    root: &'a Path,
    visited: HashSet<PathBuf>,
    errors: Vec<String>,
}

impl ModuleValidator<'_> {
    fn visit(&mut self, file: &Path, module_directory: &Path) {
        let key = canonical_or_owned(file);
        if !self.visited.insert(key) {
            return;
        }
        if !file.is_file() {
            self.errors.push(format!(
                "referenced Rust source is missing: {}",
                relative_display(self.root, file)
            ));
            return;
        }

        let source = match fs::read_to_string(file) {
            Ok(source) => source,
            Err(error) => {
                self.errors.push(format!(
                    "cannot read Rust source {}: {error}",
                    relative_display(self.root, file)
                ));
                return;
            }
        };
        let references = parse_rust_references(&source);

        for include in references.includes {
            if !include.ends_with(".rs") {
                continue;
            }
            let included = file.parent().unwrap_or(self.root).join(&include);
            if included.is_file() {
                self.visit(&included, module_directory);
            } else {
                self.errors.push(format!(
                    "include! in {} references missing file: {include}",
                    relative_display(self.root, file)
                ));
            }
        }

        for (name, path_attribute) in references.modules {
            if let Some(relative) = path_attribute {
                let target = file.parent().unwrap_or(self.root).join(&relative);
                if target.is_file() {
                    let child_directory = child_module_directory(&target);
                    self.visit(&target, &child_directory);
                } else {
                    self.errors.push(format!(
                        "module {name:?} declared by {} references missing source: {relative}",
                        relative_display(self.root, file)
                    ));
                }
                continue;
            }

            let direct = module_directory.join(format!("{name}.rs"));
            let nested = module_directory.join(&name).join("mod.rs");
            let direct_exists = direct.is_file();
            let nested_exists = nested.is_file();
            match (direct_exists, nested_exists) {
                (true, false) => self.visit(&direct, &module_directory.join(&name)),
                (false, true) => self.visit(&nested, &module_directory.join(&name)),
                (false, false) => self.errors.push(format!(
                    "module {name:?} declared by {} has no source file",
                    relative_display(self.root, file)
                )),
                (true, true) => self.errors.push(format!(
                    "module {name:?} declared by {} is ambiguous: {} and {}",
                    relative_display(self.root, file),
                    relative_display(self.root, &direct),
                    relative_display(self.root, &nested)
                )),
            }
        }
    }
}

fn child_module_directory(file: &Path) -> PathBuf {
    if file.file_name() == Some(OsStr::new("mod.rs")) {
        file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
    } else {
        let stem = file.file_stem().unwrap_or_else(|| OsStr::new("module"));
        file.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(stem)
    }
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn collect_files_with_extension(
    directory: &Path,
    extension: &str,
    output: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|error| {
        XtaskError::new(format!("cannot read directory {}: {error}", directory.display()))
    })? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_with_extension(&path, extension, output)?;
        } else if file_type.is_file() && path.extension() == Some(OsStr::new(extension)) {
            output.push(path);
        }
    }
    Ok(())
}

fn rust_string_literals(source: &str) -> impl Iterator<Item = String> + '_ {
    lex_rust(source).into_iter().filter_map(|token| match token {
        Token::String(value) => Some(value),
        _ => None,
    })
}

fn shader_include_str_paths(source: &str) -> Vec<String> {
    let tokens = lex_rust(source);
    let mut result = Vec::new();
    let mut index = 0usize;
    while index + 3 < tokens.len() {
        if token_is_ident(tokens.get(index), "include_str")
            && token_is_punct(tokens.get(index + 1), '!')
            && token_is_punct(tokens.get(index + 2), '(')
        {
            if let Some(Token::String(path)) = tokens.get(index + 3) {
                if path.ends_with(".wgsl") {
                    result.push(path.clone());
                }
            }
        }
        index += 1;
    }
    result
}

fn validate_shader_imports(root: &Path) -> Result<Vec<String>> {
    let gpu_root = root.join("crates/auraw-gpu");
    let shader_directory = gpu_root.join("src/shaders");
    if !shader_directory.is_dir() {
        return Ok(vec![
            "missing shader source directory: crates/auraw-gpu/src/shaders".to_owned(),
        ]);
    }

    let mut errors = Vec::new();
    let mut shader_names = BTreeSet::new();
    for entry in fs::read_dir(&shader_directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && path.extension() == Some(OsStr::new("wgsl")) {
            if let Some(name) = path.file_name().and_then(OsStr::to_str) {
                shader_names.insert(name.to_owned());
            }
        }
    }

    let build_rs_path = gpu_root.join("build.rs");
    let build_rs = fs::read_to_string(&build_rs_path).map_err(|error| {
        XtaskError::new(format!(
            "cannot read {}: {error}",
            relative_display(root, &build_rs_path)
        ))
    })?;
    let watched: BTreeSet<String> = rust_string_literals(&build_rs)
        .filter(|value| value.ends_with(".wgsl"))
        .filter_map(|value| {
            Path::new(&value)
                .file_name()
                .and_then(OsStr::to_str)
                .map(str::to_owned)
        })
        .collect();

    for name in shader_names.difference(&watched) {
        errors.push(format!("WGSL file is not watched by auraw-gpu/build.rs: {name}"));
    }
    for name in watched.difference(&shader_names) {
        errors.push(format!("auraw-gpu/build.rs watches a missing WGSL file: {name}"));
    }

    let mut imported = BTreeSet::new();
    let mut rust_sources = Vec::new();
    collect_files_with_extension(&gpu_root.join("src"), "rs", &mut rust_sources)?;
    for path in rust_sources {
        let source = fs::read_to_string(&path).map_err(|error| {
            XtaskError::new(format!(
                "cannot read Rust source {}: {error}",
                relative_display(root, &path)
            ))
        })?;
        for include in shader_include_str_paths(&source) {
            if let Some(name) = Path::new(&include).file_name().and_then(OsStr::to_str) {
                imported.insert(name.to_owned());
            }
        }
    }

    let roots: Vec<String> = imported.iter().cloned().collect();
    for shader in roots {
        collect_shader_imports(&shader_directory, &shader, &mut imported, &mut errors)?;
    }

    for name in shader_names.difference(&imported) {
        errors.push(format!(
            "WGSL file is not imported by auraw-gpu Rust source or a shader template: {name}"
        ));
    }

    errors.sort();
    errors.dedup();
    Ok(errors)
}

fn collect_shader_imports(
    shader_directory: &Path,
    shader_name: &str,
    imported: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) -> Result<()> {
    let path = shader_directory.join(shader_name);
    if !path.is_file() {
        errors.push(format!("shader #import references missing WGSL file: {shader_name}"));
        return Ok(());
    }
    let source = fs::read_to_string(&path)?;
    for line in source.lines() {
        let trimmed = line.trim();
        let Some(argument) = trimmed.strip_prefix("#import ") else {
            continue;
        };
        let import_path = argument
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .split("::{")
            .next()
            .unwrap_or_default();
        let Some(module_name) = import_path.strip_prefix("auraw::") else {
            errors.push(format!(
                "{shader_name} has unsupported naga_oil import path: {import_path:?}"
            ));
            continue;
        };
        if module_name.contains("::") || !is_simple_file_name(module_name) {
            errors.push(format!(
                "{shader_name} imports invalid naga_oil module: {import_path:?}"
            ));
            continue;
        }
        let imported_shader = format!("{module_name}.wgsl");
        if imported.insert(imported_shader.clone()) {
            collect_shader_imports(shader_directory, &imported_shader, imported, errors)?;
        }
    }
    Ok(())
}

fn is_simple_file_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn required_f64(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| XtaskError::new(format!("benchmark budget {key} must be a number")))
}

const ICON_BACKGROUND: Rgba<u8> = Rgba([17, 24, 39, 255]);
const ICON_FOREGROUND: Rgba<u8> = Rgba([255, 255, 255, 255]);
const ICON_OUTER_A: [(f64, f64); 7] = [
    (54.0, 18.0),
    (84.0, 88.0),
    (69.0, 88.0),
    (62.0, 70.0),
    (46.0, 70.0),
    (39.0, 88.0),
    (24.0, 88.0),
];
const ICON_INNER_A: [(f64, f64); 3] = [(51.0, 57.0), (57.0, 57.0), (54.0, 44.0)];

fn point_in_polygon(x: f64, y: f64, polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let (xi, yi) = polygon[current];
        let (xj, yj) = polygon[previous];
        if ((yi > y) != (yj > y))
            && x < (xj - xi) * (y - yi) / (yj - yi) + xi
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn render_icon(edge: u32) -> RgbaImage {
    let supersampling = 4_u32;
    let render_edge = edge * supersampling;
    let scale = f64::from(render_edge) / 108.0;
    let scaled_outer: Vec<_> = ICON_OUTER_A
        .iter()
        .map(|(x, y)| (x * scale, y * scale))
        .collect();
    let scaled_inner: Vec<_> = ICON_INNER_A
        .iter()
        .map(|(x, y)| (x * scale, y * scale))
        .collect();
    let mut image = RgbaImage::from_pixel(render_edge, render_edge, ICON_BACKGROUND);
    for y in 0..render_edge {
        for x in 0..render_edge {
            let sample_x = f64::from(x) + 0.5;
            let sample_y = f64::from(y) + 0.5;
            if point_in_polygon(sample_x, sample_y, &scaled_outer)
                && !point_in_polygon(sample_x, sample_y, &scaled_inner)
            {
                image.put_pixel(x, y, ICON_FOREGROUND);
            }
        }
    }
    image::imageops::resize(&image, edge, edge, FilterType::Lanczos3)
}

fn encode_png(image: RgbaImage) -> Result<Vec<u8>> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|error| XtaskError::new(format!("cannot encode icon PNG: {error}")))?;
    Ok(bytes.into_inner())
}

fn write_ico(path: &Path) -> Result<()> {
    let sizes = [16_u32, 24, 32, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|edge| encode_png(render_icon(*edge)))
        .collect::<Result<_>>()?;
    let mut file = File::create(path)
        .map_err(|error| XtaskError::new(format!("cannot create {}: {error}", path.display())))?;
    file.write_all(&0_u16.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&(sizes.len() as u16).to_le_bytes())?;
    let mut offset = 6_u32 + 16_u32 * sizes.len() as u32;
    for (edge, bytes) in sizes.iter().zip(&images) {
        file.write_all(&[if *edge == 256 { 0 } else { *edge as u8 }])?;
        file.write_all(&[if *edge == 256 { 0 } else { *edge as u8 }])?;
        file.write_all(&[0, 0])?;
        file.write_all(&1_u16.to_le_bytes())?;
        file.write_all(&32_u16.to_le_bytes())?;
        file.write_all(&(bytes.len() as u32).to_le_bytes())?;
        file.write_all(&offset.to_le_bytes())?;
        offset += bytes.len() as u32;
    }
    for bytes in images {
        file.write_all(&bytes)?;
    }
    Ok(())
}

fn command_icons() -> Result<()> {
    let output = workspace_root().join("packaging/icons");
    fs::create_dir_all(&output)?;
    let icon_1024 = render_icon(1024);
    DynamicImage::ImageRgba8(icon_1024)
        .save_with_format(output.join("auraw-1024.png"), ImageFormat::Png)
        .map_err(|error| XtaskError::new(format!("cannot write auraw-1024.png: {error}")))?;
    DynamicImage::ImageRgba8(render_icon(256))
        .save_with_format(output.join("auraw-256.png"), ImageFormat::Png)
        .map_err(|error| XtaskError::new(format!("cannot write auraw-256.png: {error}")))?;
    write_ico(&output.join("auraw.ico"))?;
    Ok(())
}

fn command_bench(args: BenchArgs) -> Result<()> {
    if args.runs < 1 {
        return Err(XtaskError::usage("--runs must be positive"));
    }

    let root = workspace_root();
    let renderer = rooted(&root, args.renderer);
    let output = rooted(&root, args.output);
    let budget_file = rooted(&root, args.budget_file);

    let mut scene_inputs = BTreeMap::new();
    for (name, filename, width, height) in BENCHMARK_SCENES {
        let source = root.join("regression/raw").join(filename);
        if !source.is_file() {
            return Err(XtaskError::usage(format!(
                "committed benchmark scene is missing: {}",
                source.display()
            )));
        }
        scene_inputs.insert(name, (source, width, height));
    }

    if args.dry_run {
        for (scene, (source, _, _)) in &scene_inputs {
            let target = root
                .join("target/benchmarks")
                .join(format!("{scene}-1.npz"));
            println!(
                "{}",
                display_command(
                    &renderer,
                    [
                        OsStr::new("--backend"),
                        OsStr::new("gpu"),
                        OsStr::new("--input"),
                        source.as_os_str(),
                        OsStr::new("--output"),
                        target.as_os_str(),
                    ]
                )
            );
        }
        return Ok(());
    }

    if !renderer.is_file() {
        return Err(XtaskError::usage(format!(
            "renderer does not exist: {}",
            renderer.display()
        )));
    }

    let measured_runs = args.runs as usize;
    let benchmark_directory = root.join("target/benchmarks");
    fs::create_dir_all(&benchmark_directory)?;
    let mut warmups = BTreeMap::<String, f64>::new();
    let mut measured = BTreeMap::<String, Vec<f64>>::new();

    for (scene, (source, _, _)) in &scene_inputs {
        let mut times = Vec::with_capacity(measured_runs);
        for run in 0..=measured_runs {
            let target = benchmark_directory.join(format!("{scene}-{run}.npz"));
            let elapsed_ms = run_renderer(&renderer, source, &target)?;
            if run == 0 {
                warmups.insert((*scene).to_owned(), elapsed_ms);
            } else {
                times.push(elapsed_ms);
            }
        }
        measured.insert((*scene).to_owned(), times);
    }

    let mut scene_reports = serde_json::Map::new();
    for (scene, times) in &measured {
        let (_, width, height) = &scene_inputs[scene.as_str()];
        let megapixels = f64::from(*width) * f64::from(*height) / 1_000_000.0;
        let median_ms = median(times);
        scene_reports.insert(
            scene.clone(),
            json!({
                "width": width,
                "height": height,
                "megapixels": megapixels,
                "warmup_ms": warmups[scene],
                "times_ms": times,
                "median_ms": median_ms,
                "p95_ms": legacy_percentile_95(times),
                "median_megapixels_per_second": megapixels / (median_ms / 1000.0),
            }),
        );
    }

    let budget: Value = serde_json::from_reader(File::open(&budget_file).map_err(|error| {
        XtaskError::new(format!("cannot read {}: {error}", budget_file.display()))
    })?)?;
    let budgets = budget
        .get("budgets")
        .ok_or_else(|| XtaskError::new("benchmark budget is missing budgets"))?;
    let minimum_throughput = required_f64(budgets, "export_mp_per_second_min")?;
    let maximum_startup = required_f64(budgets, "startup_shader_compile_p95_ms")?;
    let throughput_pass = scene_reports.values().all(|scene| {
        scene["median_megapixels_per_second"]
            .as_f64()
            .is_some_and(|value| value >= minimum_throughput)
    });
    let startup_pass = warmups
        .values()
        .copied()
        .reduce(f64::max)
        .is_some_and(|value| value <= maximum_startup);
    let passed = throughput_pass && startup_pass;
    let budget_display = budget_file
        .strip_prefix(&root)
        .unwrap_or(&budget_file)
        .to_string_lossy()
        .replace('\\', "/");

    let report = json!({
        "schema": 2,
        "renderer": renderer.to_string_lossy(),
        "runs": measured_runs,
        "scenes": scene_reports,
        "budget": {
            "budget_file": budget_display,
            "export_throughput_pass": throughput_pass,
            "startup_pass": startup_pass,
            "passed": passed,
        },
        "measurement_scope": "wall-clock process startup plus canonical GPU render/readback; use native GPU timestamp queries for per-pass diagnosis",
    });
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(&output)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    println!("{}", output.display());

    if args.enforce_budget && !passed {
        Err(XtaskError::silent(1))
    } else {
        Ok(())
    }
}

fn legacy_percentile_95(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let index = ((ordered.len() as f64 * 0.95) as usize).max(1) - 1;
    ordered[index]
}

fn run_renderer(renderer: &Path, input: &Path, output: &Path) -> Result<f64> {
    let started = Instant::now();
    let status = Command::new(renderer)
        .args(["--backend", "gpu", "--input"])
        .arg(input)
        .arg("--output")
        .arg(output)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            XtaskError::new(format!("could not execute {}: {error}", renderer.display()))
        })?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    if !status.success() {
        return Err(XtaskError::silent(status.code().unwrap_or(1)));
    }
    Ok(elapsed_ms)
}

fn median(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let middle = ordered.len() / 2;
    if ordered.len() % 2 == 0 {
        (ordered[middle - 1] + ordered[middle]) / 2.0
    } else {
        ordered[middle]
    }
}

fn percentile_95(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let rank = (ordered.len() * 95).div_ceil(100).max(1);
    ordered[rank - 1]
}

fn display_command<I, S>(program: &Path, arguments: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    std::iter::once(shell_escape(program.as_os_str()))
        .chain(arguments.into_iter().map(|argument| shell_escape(argument.as_ref())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_./:=+-".contains(character))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn command_verify_android_16kb(args: AndroidArgs) -> Result<()> {
    let contract = load_build_contract()?;
    if args.print_build_contract {
        return print_build_contract(&contract);
    }
    let root = workspace_root();
    let apk = rooted(&root, args.apk);
    if !apk.is_file() {
        return Err(XtaskError::new(format!("APK not found: {}", apk.display())));
    }

    let sdk = android_sdk_root_with_local_properties(&root)?.ok_or_else(|| {
        XtaskError::new("Android SDK not found. Set ANDROID_SDK_ROOT (or ANDROID_HOME).")
    })?;
    let ndk = android_ndk_root(&root, &contract.ndk_version, false)?;
    let ndk_host = ndk_host_root(&ndk)?;
    let objdump = args
        .objdump
        .or_else(|| env::var_os("LLVM_OBJDUMP").map(PathBuf::from))
        .unwrap_or_else(|| ndk_host.join("bin").join(executable_name("llvm-objdump")));
    let zipalign = args
        .zipalign
        .or_else(|| env::var_os("ZIPALIGN").map(PathBuf::from))
        .unwrap_or_else(|| {
            sdk.join("build-tools")
                .join(&contract.build_tools_version)
                .join(executable_name("zipalign"))
        });
    if !objdump.is_file() {
        return Err(XtaskError::new(format!("llvm-objdump not found: {}", objdump.display())));
    }
    if !zipalign.is_file() {
        return Err(XtaskError::new(format!(
            "zipalign {} not found: {}",
            contract.build_tools_version,
            zipalign.display()
        )));
    }

    let temporary = TemporaryDirectory::new("auraw-16kb")?;
    let libraries = extract_64_bit_libraries(&apk, temporary.path())?;
    if libraries.is_empty() {
        println!("No 64-bit native libraries found; ELF 16 KB check not applicable.");
    }
    for (archive_path, library) in &libraries {
        verify_elf_alignment(&objdump, archive_path, library)?;
        println!("16 KB ELF aligned: {archive_path}");
    }

    run_checked(
        Command::new(&zipalign)
            .args(["-c", "-P", "16", "-v", "4"])
            .arg(&apk),
        &zipalign.display().to_string(),
    )?;
    println!("Android 16 KB page-size checks passed: {}", apk.display());
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum MetadataKind {
    String,
    PositiveInteger,
    Boolean,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildContract {
    ndk_version: String,
    build_tools_version: String,
    compile_sdk: u64,
    min_sdk: u64,
    target_sdk: u64,
    libraw_revision: String,
    lensfun_revision: String,
    use_legacy_packaging: bool,
}

impl BuildContract {
    fn value(&self, field: &str) -> Option<String> {
        match field {
            "ndkVersion" => Some(self.ndk_version.clone()),
            "buildToolsVersion" => Some(self.build_tools_version.clone()),
            "compileSdk" => Some(self.compile_sdk.to_string()),
            "minSdk" => Some(self.min_sdk.to_string()),
            "targetSdk" => Some(self.target_sdk.to_string()),
            "librawRevision" => Some(self.libraw_revision.clone()),
            "lensfunRevision" => Some(self.lensfun_revision.clone()),
            "useLegacyPackaging" => Some(self.use_legacy_packaging.to_string()),
            _ => None,
        }
    }
}

fn load_build_contract() -> Result<BuildContract> {
    let root = workspace_root();
    let manifest = root.join("Cargo.toml");
    let metadata = read_workspace_metadata(&manifest)?;
    validate_required_workspace_metadata(&manifest, &metadata)?;
    Ok(BuildContract {
        ndk_version: metadata["android_ndk_version"].as_str().unwrap().to_owned(),
        build_tools_version: metadata["android_build_tools_version"]
            .as_str()
            .unwrap()
            .to_owned(),
        compile_sdk: metadata["android_compile_sdk"].as_u64().unwrap(),
        min_sdk: metadata["android_min_sdk"].as_u64().unwrap(),
        target_sdk: metadata["android_target_sdk"].as_u64().unwrap(),
        libraw_revision: metadata["libraw_revision"].as_str().unwrap().to_owned(),
        lensfun_revision: metadata["lensfun_revision"].as_str().unwrap().to_owned(),
        use_legacy_packaging: metadata["android_use_legacy_packaging"].as_bool().unwrap(),
    })
}

fn command_print_metadata(args: MetadataArgs) -> Result<()> {
    let contract = load_build_contract()?;
    if let Some(field) = args.value {
        println!("{}", contract.value(&field).ok_or_else(|| {
            XtaskError::usage(format!("unknown build metadata field: {field}"))
        })?);
    } else {
        match args.format {
            MetadataFormat::Json => println!("{}", serde_json::to_string(&contract)?),
            MetadataFormat::Shell => {
                let environment = [
                    ("AURAW_ANDROID_NDK_VERSION", contract.ndk_version.clone()),
                    (
                        "AURAW_ANDROID_BUILD_TOOLS_VERSION",
                        contract.build_tools_version.clone(),
                    ),
                    ("AURAW_ANDROID_COMPILE_SDK", contract.compile_sdk.to_string()),
                    ("AURAW_ANDROID_MIN_SDK", contract.min_sdk.to_string()),
                    ("AURAW_ANDROID_TARGET_SDK", contract.target_sdk.to_string()),
                    ("AURAW_LIBRAW_REVISION", contract.libraw_revision.clone()),
                    ("AURAW_LENSFUN_REVISION", contract.lensfun_revision.clone()),
                    (
                        "AURAW_ANDROID_USE_LEGACY_PACKAGING",
                        contract.use_legacy_packaging.to_string(),
                    ),
                ];
                for (key, value) in environment {
                    println!("export {key}={}", shell_escape(OsStr::new(&value)));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum AndroidDependency {
    LibRaw,
    Lensfun,
}

fn print_build_contract(contract: &BuildContract) -> Result<()> {
    println!("{}", serde_json::to_string(contract)?);
    Ok(())
}

fn android_abi_config(abi: &str, api: u64) -> Result<(String, &'static str)> {
    match abi {
        "arm64-v8a" => Ok((format!("aarch64-linux-android{api}"), "aarch64-linux-android")),
        "x86_64" => Ok((format!("x86_64-linux-android{api}"), "x86_64-linux-android")),
        _ => Err(XtaskError::usage(format!(
            "Unsupported ABI '{abi}' (use arm64-v8a or x86_64)"
        ))),
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        let direct = directory.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) {
            let executable = directory.join(format!("{name}.exe"));
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

fn require_executable(name: &str, message: &str) -> Result<PathBuf> {
    find_executable(name).ok_or_else(|| XtaskError::new(message))
}

fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| XtaskError::new(format!("unable to execute {description}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(XtaskError::with_code(String::new(), status.code().unwrap_or(1)))
    }
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_symlink() || path.is_file() {
        fs::remove_file(path)?;
    } else if path.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn require_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(XtaskError::new(format!(
            "required file was not produced: {}",
            path.display()
        )))
    }
}

fn directory_has_extension(path: &Path, extension: &str) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let candidate = entry.path();
            if candidate.is_dir() {
                stack.push(candidate);
            } else if candidate.extension() == Some(OsStr::new(extension)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn android_sdk_root_with_local_properties(root: &Path) -> Result<Option<PathBuf>> {
    if let Some(configured) = env::var_os("ANDROID_SDK_ROOT").or_else(|| env::var_os("ANDROID_HOME")) {
        return Ok(Some(rooted(root, configured)));
    }
    let local_properties = root.join("android/local.properties");
    if !local_properties.is_file() {
        return Ok(None);
    }
    let properties = parse_properties_file(&local_properties)?;
    Ok(properties.get("sdk.dir").map(|value| rooted(root, value)))
}

fn android_ndk_root(root: &Path, expected_version: &str, require_toolchain: bool) -> Result<PathBuf> {
    let sdk = android_sdk_root_with_local_properties(root)?;
    let configured = env::var_os("ANDROID_NDK_HOME").or_else(|| env::var_os("ANDROID_NDK_ROOT"));
    let ndk = configured
        .map(|path| rooted(root, path))
        .or_else(|| sdk.map(|sdk| sdk.join("ndk").join(expected_version)))
        .ok_or_else(|| {
            XtaskError::new("Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT).")
        })?;
    if require_toolchain && !ndk.join("build/cmake/android.toolchain.cmake").is_file() {
        return Err(XtaskError::new(
            "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT).",
        ));
    }
    let source_properties = ndk.join("source.properties");
    if !source_properties.is_file() {
        return Err(XtaskError::new(format!("Android NDK not found at {}", ndk.display())));
    }
    let properties = parse_properties_file(&source_properties)?;
    let revision = properties.get("Pkg.Revision").map(String::as_str).unwrap_or("");
    if revision != expected_version {
        return Err(XtaskError::new(format!(
            "Android NDK {expected_version} is required, found {} at {}",
            if revision.is_empty() { "unknown" } else { revision },
            ndk.display()
        )));
    }
    Ok(ndk)
}

fn ndk_host_root(ndk: &Path) -> Result<PathBuf> {
    let prebuilt = ndk.join("toolchains/llvm/prebuilt");
    let mut candidates = if prebuilt.is_dir() {
        fs::read_dir(&prebuilt)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    candidates.sort();
    candidates.into_iter().next().ok_or_else(|| {
        XtaskError::new(format!("The selected NDK has no LLVM toolchain: {}", ndk.display()))
    })
}

fn find_named_file_recursive(root: &Path, prefix: &str) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = fs::read_dir(directory).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(prefix))
            {
                return Some(path);
            }
        }
    }
    None
}

fn find_host_libclang(ndk_host: &Path) -> Option<PathBuf> {
    for candidate in [ndk_host.join("lib64"), ndk_host.join("lib")] {
        if find_named_file_recursive(&candidate, "libclang.so").is_some() {
            return Some(candidate);
        }
    }
    find_named_file_recursive(Path::new("/usr/lib"), "libclang.so")
        .and_then(|library| library.parent().map(Path::to_path_buf))
}

fn run_gradle_android_native_dependencies(root: &Path, abi: &str, profile: &str, min_sdk: u64) -> Result<()> {
    android_abi_config(abi, min_sdk)?;
    if !matches!(profile, "debug" | "release") {
        return Err(XtaskError::usage(format!(
            "Unknown profile '{profile}' (use release or debug)"
        )));
    }
    let gradlew = root.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" });
    require_file(&gradlew)?;
    let mut title = profile.to_owned();
    if let Some(first) = title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    let task = format!(":app:externalNativeBuild{title}");
    run_checked(
        Command::new(&gradlew)
            .current_dir(root)
            .arg(task)
            .arg(format!("-PaurawAbis={abi}"))
            .arg("-PaurawBuildRust=false"),
        &gradlew.display().to_string(),
    )
}

fn source_date_epoch(root: &Path, revision: &str) -> Result<String> {
    Ok(run_command_output(
        Command::new("git")
            .args(["-C"])
            .arg(root)
            .args(["show", "-s", "--format=%ct", revision]),
        "git show source date",
    )?
    .trim()
    .to_owned())
}

fn release_build_environment(root: &Path, revision: &str) -> Result<BTreeMap<OsString, OsString>> {
    let mut environment: BTreeMap<OsString, OsString> = env::vars_os().collect();
    environment.insert("AURAW_REQUIRE_COMMITTED_SOURCE".into(), "1".into());
    environment.insert("AURAW_SOURCE_REVISION".into(), revision.into());
    environment.insert("SOURCE_DATE_EPOCH".into(), source_date_epoch(root, revision)?.into());
    environment.insert("CARGO_INCREMENTAL".into(), "0".into());
    environment.insert("CARGO_TARGET_DIR".into(), root.join("target").into_os_string());
    for key in ["CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS"] {
        environment.remove(OsStr::new(key));
    }
    Ok(environment)
}

fn command_build_android_dependency(args: BuildDependencyArgs, dependency: AndroidDependency) -> Result<()> {
    let contract = load_build_contract()?;
    if args.print_build_contract {
        return print_build_contract(&contract);
    }
    let root = workspace_root();
    run_gradle_android_native_dependencies(&root, &args.abi, "release", contract.min_sdk)?;
    match dependency {
        AndroidDependency::LibRaw => {
            let staged = root.join("android/native/libraw").join(&args.abi);
            require_file(&staged.join("include/libraw/libraw.h"))?;
            require_file(&staged.join("lib/libraw.a"))?;
            println!("AGP/CMake staged LibRaw for {} in {}", args.abi, staged.display());
        }
        AndroidDependency::Lensfun => {
            let staged = root.join("android/native/lensfun").join(&args.abi);
            require_file(&staged.join("include/lensfun/lensfun.h"))?;
            require_file(&staged.join("lib/liblensfun.a"))?;
            require_file(&staged.join("lib/libglib-2.0.a"))?;
            let assets = staged.join("apk-assets/lensfun");
            if !directory_has_extension(&assets, "xml")? {
                return Err(XtaskError::new(format!(
                    "Lensfun XML database is missing from {}",
                    assets.display()
                )));
            }
            println!("AGP/CMake staged Lensfun for {} in {}", args.abi, staged.display());
        }
    }
    Ok(())
}

fn command_build_android(args: BuildAndroidArgs) -> Result<()> {
    let contract = load_build_contract()?;
    if args.print_build_contract {
        return print_build_contract(&contract);
    }
    let root = workspace_root();
    let (clang_target, cxx_triple) = android_abi_config(&args.abi, contract.min_sdk)?;
    if !matches!(args.profile.as_str(), "debug" | "release") {
        return Err(XtaskError::usage(format!(
            "Unknown profile '{}' (use release or debug)",
            args.profile
        )));
    }
    let ndk = android_ndk_root(&root, &contract.ndk_version, true)?;
    let ndk_host = ndk_host_root(&ndk)?;
    let sysroot = ndk_host.join("sysroot");
    if !sysroot.is_dir() {
        return Err(XtaskError::new(format!(
            "The selected NDK has no LLVM sysroot: {}",
            ndk.display()
        )));
    }
    require_executable(
        "cargo-ndk",
        "cargo-ndk 4.1.2 is required. Install it with: cargo install cargo-ndk --version 4.1.2 --locked",
    )?;
    let cargo = require_executable("cargo", "cargo is required")?;
    let mut base_environment: BTreeMap<OsString, OsString> = env::vars_os().collect();
    base_environment.insert("ANDROID_NDK_HOME".into(), ndk.clone().into_os_string());
    base_environment.insert(
        "BINDGEN_EXTRA_CLANG_ARGS".into(),
        format!("--target={clang_target} --sysroot={}", sysroot.display()).into(),
    );
    if !base_environment.contains_key(OsStr::new("LIBCLANG_PATH")) {
        if let Some(libclang) = find_host_libclang(&ndk_host) {
            base_environment.insert("LIBCLANG_PATH".into(), libclang.into_os_string());
        }
    }
    let version_output = run_command_output(
        Command::new(&cargo)
            .arg("ndk")
            .arg("--version")
            .env_clear()
            .envs(&base_environment),
        "cargo ndk --version",
    )?;
    let cargo_ndk_version = version_output.trim().strip_prefix("cargo-ndk ").unwrap_or(version_output.trim());
    if cargo_ndk_version != "4.1.2" {
        return Err(XtaskError::new(format!(
            "cargo-ndk 4.1.2 is required, found {}",
            if cargo_ndk_version.is_empty() { "unknown" } else { cargo_ndk_version }
        )));
    }
    if !base_environment.contains_key(OsStr::new("LIBCLANG_PATH")) {
        if let Some(ldconfig) = find_executable("ldconfig") {
            if let Ok(output) = Command::new(ldconfig).arg("-p").output() {
                if !String::from_utf8_lossy(&output.stdout).contains("libclang.so") {
                    eprintln!("Warning: bindgen needs host libclang; install libclang-dev or set LIBCLANG_PATH if the build cannot find it.");
                }
            }
        }
    }

    let revision = if args.profile == "release" {
        Some(verify_source_revision(false)?)
    } else {
        None
    };
    let mut build_environment = if let Some(revision) = revision.as_deref() {
        let mut release = release_build_environment(&root, revision)?;
        for key in ["ANDROID_NDK_HOME", "BINDGEN_EXTRA_CLANG_ARGS", "LIBCLANG_PATH"] {
            if let Some(value) = base_environment.get(OsStr::new(key)) {
                release.insert(key.into(), value.clone());
            }
        }
        release
    } else {
        base_environment
    };
    build_environment.insert("CARGO_INCREMENTAL".into(), "0".into());
    build_environment.insert("CARGO_TARGET_DIR".into(), root.join("target").into_os_string());
    for key in ["CARGO_BUILD_TARGET", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS"] {
        build_environment.remove(OsStr::new(key));
    }
    if env::var_os("AURAW_NATIVE_DEPS_READY").as_deref() != Some(OsStr::new("1")) {
        run_gradle_android_native_dependencies(&root, &args.abi, &args.profile, contract.min_sdk)?;
    }
    let libraw_root = root.join("android/native/libraw").join(&args.abi);
    let lensfun_root = root.join("android/native/lensfun").join(&args.abi);
    build_environment.insert("AURAW_LIBRAW_ROOT".into(), libraw_root.into_os_string());
    build_environment.insert("AURAW_LENSFUN_ROOT".into(), lensfun_root.clone().into_os_string());
    let jni_root = root.join("android/app/src/main/jniLibs");
    let abi_jni = jni_root.join(&args.abi);
    remove_path(&abi_jni)?;
    let mut command = Command::new(&cargo);
    command
        .current_dir(&root)
        .env_clear()
        .envs(&build_environment)
        .args(["ndk", "-t"])
        .arg(&args.abi)
        .arg("-o")
        .arg(&jni_root)
        .args(["build", "--locked"]);
    if args.profile == "release" {
        command.arg("--release");
    }
    command
        .args(["--package", "auraw-ui", "--lib", "--manifest-path"])
        .arg(root.join("Cargo.toml"));
    run_checked(&mut command, &cargo.display().to_string())?;

    let cxx_runtime = ndk_host
        .join("sysroot/usr/lib")
        .join(cxx_triple)
        .join("libc++_shared.so");
    require_file(&cxx_runtime)?;
    fs::create_dir_all(&abi_jni)?;
    fs::copy(&cxx_runtime, abi_jni.join("libc++_shared.so"))?;
    require_file(&abi_jni.join("libauraw.so"))?;
    require_file(&abi_jni.join("libc++_shared.so"))?;
    let lensfun_assets = lensfun_root.join("apk-assets/lensfun");
    if !directory_has_extension(&lensfun_assets, "xml")? {
        return Err(XtaskError::new(format!(
            "Lensfun XML database is missing from {}",
            lensfun_assets.display()
        )));
    }
    if let Some(expected_revision) = revision {
        match verify_source_revision(false) {
            Ok(final_revision) if final_revision == expected_revision => {}
            Ok(_) => {
                remove_path(&abi_jni)?;
                return Err(XtaskError::new(
                    "source changed during the build; discarded the Android native library",
                ));
            }
            Err(error) => {
                remove_path(&abi_jni)?;
                eprintln!("source changed during the build; discarded the Android native library");
                return Err(error);
            }
        }
    }
    println!(
        "Rust, LibRaw, and Lensfun Android libraries are ready for Gradle ({}, {}).",
        args.abi, args.profile
    );
    Ok(())
}

fn command_build_linux() -> Result<()> {
    let root = workspace_root();
    let revision = verify_source_revision(false)?;
    let environment = release_build_environment(&root, &revision)?;
    let cargo = require_executable("cargo", "cargo is required")?;
    run_checked(
        Command::new(&cargo)
            .current_dir(&root)
            .env_clear()
            .envs(&environment)
            .args(["build", "--locked", "--release", "--manifest-path"])
            .arg(root.join("Cargo.toml")),
        &cargo.display().to_string(),
    )?;
    let outputs = [
        root.join("target/release/auraw"),
        root.join("target/release/auraw-regression-render"),
    ];
    for output in &outputs {
        require_file(output)?;
    }
    match verify_source_revision(false) {
        Ok(final_revision) if final_revision == revision => {}
        Ok(_) => {
            for output in &outputs {
                let _ = fs::remove_file(output);
            }
            return Err(XtaskError::new(
                "source changed during the build; discarded the Linux binary",
            ));
        }
        Err(error) => {
            for output in &outputs {
                let _ = fs::remove_file(output);
            }
            eprintln!("source changed during the build; discarded the Linux binary");
            return Err(error);
        }
    }
    println!("Built AuRaw from {revision}");
    Ok(())
}

fn command_verified_download(args: VerifiedDownloadArgs) -> Result<()> {
    if !args.url.starts_with("https://") {
        return Err(XtaskError::usage(format!(
            "refusing non-HTTPS download: {}",
            args.url
        )));
    }
    let root = workspace_root();
    let (algorithm, expected) = parse_expected_digest(&args.expected_digest)?;
    let output = rooted(&root, args.output);

    if output.is_file() && digest_file(&output, algorithm)? == expected {
        println!("verified cached download: {}", output.display());
        return Ok(());
    }

    let file_name = output
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("download");
    let temporary = output.with_file_name(format!(
        "{file_name}.download.{}",
        std::process::id()
    ));
    let cleanup = TemporaryFile::new(temporary.clone());
    let previous_permissions = fs::metadata(&output).ok().map(|metadata| metadata.permissions());
    download_https(&args.url, &temporary, 9, 900)?;

    let actual = digest_file(&temporary, algorithm)?;
    if actual != expected {
        eprintln!(
            "{} checksum mismatch for {}",
            algorithm.name(),
            temporary.display()
        );
        eprintln!("expected: {expected}");
        eprintln!("actual:   {actual}");
        return Err(XtaskError::silent(1));
    }
    if let Some(permissions) = previous_permissions {
        fs::set_permissions(&temporary, permissions)?;
    }
    replace_file(&temporary, &output)?;
    drop(cleanup);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum DigestAlgorithm {
    Sha256,
    Sha512,
}

impl DigestAlgorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }

    fn hex_length(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }
}

fn parse_expected_digest(source: &str) -> Result<(DigestAlgorithm, String)> {
    let (algorithm, expected) = if source.starts_with("https://") {
        let temporary = TemporaryDirectory::new("auraw-checksum")?;
        let target = temporary.path().join("checksum.txt");
        download_https(source, &target, 9, 300)?;
        let text = fs::read_to_string(&target).map_err(|error| {
            XtaskError::new(format!("cannot read checksum response from {source}: {error}"))
        })?;
        (DigestAlgorithm::Sha256, first_hex_digest(&text, 64).unwrap_or_default())
    } else if let Some(value) = source.strip_prefix("sha256:") {
        (DigestAlgorithm::Sha256, value.to_owned())
    } else if let Some(value) = source.strip_prefix("sha512:") {
        (DigestAlgorithm::Sha512, value.to_owned())
    } else {
        (DigestAlgorithm::Sha256, source.to_owned())
    };

    if expected.is_empty() || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(XtaskError::usage("invalid checksum value"));
    }
    if expected.len() != algorithm.hex_length() {
        return Err(XtaskError::usage(format!(
            "{} must contain {} hex digits",
            algorithm.label(),
            algorithm.hex_length()
        )));
    }
    Ok((algorithm, expected.to_ascii_lowercase()))
}

fn first_hex_digest(text: &str, length: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        while start < bytes.len() && !bytes[start].is_ascii_hexdigit() {
            start += 1;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        if end.saturating_sub(start) >= length {
            return String::from_utf8(bytes[start..start + length].to_vec()).ok();
        }
        start = end.saturating_add(1);
    }
    None
}

fn download_https(
    url: &str,
    destination: &Path,
    attempts: usize,
    timeout_seconds: usize,
) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(XtaskError::usage(format!("refusing non-HTTPS download: {url}")));
    }
    let retry_count = attempts.saturating_sub(1).to_string();
    let timeout = timeout_seconds.max(1).to_string();
    let status = Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "--http1.1",
            "--fail",
            "--location",
            "--show-error",
            "--retry",
            retry_count.as_str(),
            "--retry-all-errors",
            "--retry-delay",
            "3",
            "--connect-timeout",
            "30",
            "--max-time",
            timeout.as_str(),
            url,
            "-o",
        ])
        .arg(destination)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| XtaskError::new(format!("download failed for {url}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        let _ = fs::remove_file(destination);
        Err(XtaskError::with_code(
            format!("download failed for {url}: curl exited with {status}"),
            status.code().unwrap_or(1),
        ))
    }
}

fn digest_file(path: &Path, algorithm: DigestAlgorithm) -> Result<String> {
    let file = File::open(path).map_err(|error| {
        XtaskError::new(format!(
            "{} checksum could not be read for {}: {error}",
            algorithm.name(),
            path.display()
        ))
    })?;
    let digest_algorithm = match algorithm {
        DigestAlgorithm::Sha256 => &SHA256,
        DigestAlgorithm::Sha512 => &SHA512,
    };
    let mut context = DigestContext::new(digest_algorithm);
    let mut reader = io::BufReader::new(file);
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            XtaskError::new(format!(
                "{} checksum could not be read for {}: {error}",
                algorithm.name(),
                path.display()
            ))
        })?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }
    Ok(hex_encode(context.finish().as_ref()))
}

fn replace_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        if destination.exists() {
            fs::remove_file(destination).map_err(|error| {
                XtaskError::new(format!(
                    "cannot replace existing download {}: {error}",
                    destination.display()
                ))
            })?;
        }
    }
    fs::rename(source, destination).map_err(|error| {
        XtaskError::new(format!(
            "cannot move verified download to {}: {error}",
            destination.display()
        ))
    })
}

fn verify_source_revision(print_revision: bool) -> Result<String> {
    let root = workspace_root();
    let inside = Command::new("git")
        .args(["-C"])
        .arg(&root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match inside {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(XtaskError::new("git is required to verify the source revision"));
        }
        Err(error) => return Err(XtaskError::new(error.to_string())),
        Ok(status) if !status.success() => {
            return Err(XtaskError::new("release builds must run from a Git checkout"));
        }
        Ok(_) => {}
    }

    let status = run_command_output(
        Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args(["status", "--porcelain=v1", "--untracked-files=all"]),
        "git status",
    )?;
    let status = status.trim();
    if !status.is_empty() {
        eprintln!("release builds require a clean source tree:");
        eprintln!("{status}");
        return Err(XtaskError::silent(1));
    }

    let revision = run_command_output(
        Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args(["rev-parse", "--verify", "HEAD"]),
        "git rev-parse HEAD",
    )?
    .trim()
    .to_owned();
    if print_revision {
        println!("{revision}");
    }
    Ok(revision)
}

fn command_verify_source_revision() -> Result<()> {
    verify_source_revision(true).map(|_| ())
}

fn run_command_output(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .map_err(|error| XtaskError::new(format!("could not execute {description}: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(XtaskError::with_code(
            if stderr.is_empty() {
                format!("{description} failed")
            } else {
                format!("{description} failed: {stderr}")
            },
            output.status.code().unwrap_or(1),
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| XtaskError::new(format!("{description} produced non-UTF-8 output: {error}")))
}

fn read_workspace_metadata(path: &Path) -> Result<BTreeMap<String, Value>> {
    let root = path.parent().ok_or_else(|| {
        XtaskError::new(format!("workspace manifest has no parent: {}", path.display()))
    })?;
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|error| XtaskError::new(format!("could not execute cargo metadata: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(XtaskError::with_code(
            if stderr.is_empty() {
                format!("cannot read [workspace.metadata] from {}", path.display())
            } else {
                format!("cannot read [workspace.metadata] from {}: {stderr}", path.display())
            },
            output.status.code().unwrap_or(1),
        ));
    }

    let document: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        XtaskError::new(format!("cargo metadata returned invalid JSON: {error}"))
    })?;
    let metadata = document
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            XtaskError::new(format!(
                "{}: missing or invalid [workspace.metadata] table",
                path.display()
            ))
        })?;
    Ok(metadata
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

fn validate_required_workspace_metadata(
    path: &Path,
    values: &BTreeMap<String, Value>,
) -> Result<()> {
    let missing = REQUIRED_WORKSPACE_METADATA
        .iter()
        .filter_map(|(key, _)| (!values.contains_key(*key)).then_some(*key))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(XtaskError::new(format!(
            "{}: [workspace.metadata] is missing required key(s): {}",
            path.display(),
            missing.join(", ")
        )));
    }

    for (key, kind) in REQUIRED_WORKSPACE_METADATA {
        let value = &values[key];
        let valid = match kind {
            MetadataKind::String => value.as_str().is_some_and(|value| !value.is_empty()),
            MetadataKind::PositiveInteger => value.as_u64().is_some_and(|value| value > 0),
            MetadataKind::Boolean => value.is_boolean(),
        };
        if !valid {
            let expected = match kind {
                MetadataKind::String => "a non-empty string",
                MetadataKind::PositiveInteger => "a positive integer",
                MetadataKind::Boolean => "a boolean",
            };
            return Err(XtaskError::new(format!(
                "{}: [workspace.metadata].{key} must be {expected}",
                path.display()
            )));
        }
    }

    let min_sdk = values["android_min_sdk"].as_u64().unwrap_or_default();
    let target_sdk = values["android_target_sdk"].as_u64().unwrap_or_default();
    let compile_sdk = values["android_compile_sdk"].as_u64().unwrap_or_default();
    if min_sdk > target_sdk {
        return Err(XtaskError::new(format!(
            "{}: [workspace.metadata].android_min_sdk cannot exceed android_target_sdk",
            path.display()
        )));
    }
    if target_sdk > compile_sdk {
        return Err(XtaskError::new(format!(
            "{}: [workspace.metadata].android_target_sdk cannot exceed android_compile_sdk",
            path.display()
        )));
    }
    Ok(())
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667,
                0xbb67ae85,
                0x3c6ef372,
                0xa54ff53a,
                0x510e527f,
                0x9b05688c,
                0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.total_len = self.total_len.wrapping_add(input.len() as u64);
        if self.buffer_len != 0 {
            let needed = 64 - self.buffer_len;
            let copied = needed.min(input.len());
            self.buffer[self.buffer_len..self.buffer_len + copied]
                .copy_from_slice(&input[..copied]);
            self.buffer_len += copied;
            input = &input[copied..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
        while input.len() >= 64 {
            let block: &[u8; 64] = input[..64].try_into().expect("64-byte SHA-256 block");
            self.compress(block);
            input = &input[64..];
        }
        if !input.is_empty() {
            self.buffer[..input.len()].copy_from_slice(input);
            self.buffer_len = input.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer = [0; 64];
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut digest = [0u8; 32];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
            0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
            0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
            0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
            0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
            0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
            0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
            0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut schedule = [0u32; 64];
        for (index, chunk) in block.chunks_exact(4).take(16).enumerate() {
            schedule[index] = u32::from_be_bytes(chunk.try_into().expect("four-byte word"));
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sigma1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sigma0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(value);
        }
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .map_err(|error| XtaskError::new(format!("cannot open {}: {error}", path.display())))?;
    let mut reader = io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| XtaskError::new(format!("cannot read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        let _ = fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn executable_name(name: &str) -> OsString {
    if cfg!(windows) {
        OsString::from(format!("{name}.exe"))
    } else {
        OsString::from(name)
    }
}

fn extract_64_bit_libraries(apk: &Path, destination: &Path) -> Result<Vec<(String, PathBuf)>> {
    let file = File::open(apk)
        .map_err(|error| XtaskError::new(format!("cannot open APK {}: {error}", apk.display())))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| XtaskError::new(format!("invalid APK {}: {error}", apk.display())))?;
    let mut libraries = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let archive_path = entry.name().replace('\\', "/");
        let Some((abi, file_name)) = native_library_path(&archive_path) else {
            continue;
        };
        if !ANDROID_64_BIT_ABIS.contains(&abi) {
            continue;
        }

        let abi_directory = destination.join(abi);
        fs::create_dir_all(&abi_directory)?;
        let output_path = abi_directory.join(file_name);
        let mut output = File::create(&output_path)?;
        io::copy(&mut entry, &mut output)?;
        libraries.push((archive_path, output_path));
    }

    libraries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(libraries)
}

fn native_library_path(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.split('/');
    let root = parts.next()?;
    let abi = parts.next()?;
    let file = parts.next()?;
    if root != "lib" || parts.next().is_some() || !file.ends_with(".so") || file.is_empty() {
        return None;
    }
    if !is_simple_file_name(abi) || !is_simple_file_name(file) {
        return None;
    }
    Some((abi, file))
}

fn verify_elf_alignment(objdump: &Path, _archive_path: &str, library: &Path) -> Result<()> {
    let output = Command::new(objdump)
        .arg("-p")
        .arg(library)
        .output()
        .map_err(|error| XtaskError::new(format!("could not execute {}: {error}", objdump.display())))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let load_lines: Vec<&str> = stdout.lines().filter(|line| line.contains("LOAD")).collect();
    let alignments: Vec<u32> = load_lines
        .iter()
        .filter_map(|line| parse_alignment_power(line))
        .collect();
    if !output.status.success() || alignments.is_empty() {
        return Err(XtaskError::new(format!(
            "Could not read ELF LOAD alignment from {}",
            library.display()
        )));
    }
    if alignments.iter().any(|alignment| *alignment < 14) {
        eprintln!("16 KB ELF alignment check failed: {}", library.display());
        for line in load_lines {
            eprintln!("{line}");
        }
        return Err(XtaskError::silent(1));
    }
    Ok(())
}

fn parse_alignment_power(line: &str) -> Option<u32> {
    let marker = "align 2**";
    let start = line.find(marker)? + marker.len();
    let digits: String = line[start..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(prefix: &str) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|error| {
            XtaskError::new(format!("cannot create temporary directory {}: {error}", path.display()))
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hex_encode, median, native_library_path, parse_alignment_power, percentile_95,
        validate_generated_binaries, Sha256, TemporaryDirectory,
    };
    use std::fs;

    #[test]
    fn statistics_are_deterministic() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        assert_eq!(percentile_95(&[1.0, 2.0, 3.0]), 3.0);
    }

    #[test]
    fn parses_llvm_objdump_alignment() {
        assert_eq!(parse_alignment_power("  LOAD off 0x0 align 2**14"), Some(14));
        assert_eq!(parse_alignment_power("  LOAD off 0x0 align 4096"), None);
    }

    #[test]
    fn sha256_matches_standard_test_vector() {
        let mut hasher = Sha256::new();
        hasher.update(b"abc");
        assert_eq!(
            hex_encode(&hasher.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn ignores_android_cmake_outputs_but_rejects_source_binaries() {
        let directory = TemporaryDirectory::new("auraw-generated-binary-test").unwrap();
        let generated = directory
            .path()
            .join("android/app/.cxx/Debug/arm64-v8a/CMakeFiles/raw.dir/colorconst.cpp.o");
        fs::create_dir_all(generated.parent().unwrap()).unwrap();
        fs::write(&generated, b"generated object").unwrap();

        assert!(validate_generated_binaries(directory.path()).unwrap().is_empty());

        let source_binary = directory.path().join("crates/auraw-core/src/accidental.o");
        fs::create_dir_all(source_binary.parent().unwrap()).unwrap();
        fs::write(&source_binary, b"unexpected object").unwrap();

        assert_eq!(
            validate_generated_binaries(directory.path()).unwrap(),
            vec![String::from(concat!(
                "generated binary is present in the source tree: ",
                "crates/auraw-core/src/accidental.o"
            ))]
        );
    }

    #[test]
    fn accepts_only_flat_apk_native_library_paths() {
        assert_eq!(
            native_library_path("lib/arm64-v8a/libauraw.so"),
            Some(("arm64-v8a", "libauraw.so"))
        );
        assert_eq!(native_library_path("assets/libauraw.so"), None);
        assert_eq!(native_library_path("lib/arm64-v8a/sub/libauraw.so"), None);
    }
}
