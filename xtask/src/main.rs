use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

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
const IGNORED_BINARY_ROOTS: [&str; 8] = [
    ".git",
    ".gradle",
    "dist",
    "target",
    "android/.gradle",
    "android/build",
    "android/app/build",
    "android/native",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{RED}{BOLD}FAIL{RESET} {error}");
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
            command_check_all()
        }
        "print-metadata" => command_print_metadata(parse_print_metadata_args(rest)?),
        "verified-download" => command_verified_download(parse_verified_download_args(rest)?),
        "verify-source-revision" => {
            ensure_no_extra_args(&rest, "verify-source-revision")?;
            command_verify_source_revision()
        }
        "bench" => command_bench(parse_bench_args(rest)?),
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
        "{BOLD}AuRaw developer tasks{RESET}\n\n\
         {BOLD}USAGE{RESET}\n  cargo xtask <COMMAND> [OPTIONS]\n\n\
         {BOLD}COMMANDS{RESET}\n  check-all\n      Validate the source tree, immutable workflow pins, and Gradle wrapper.\n\n\
         print-metadata [--format json|shell]\n      Print the root Cargo.toml [workspace.metadata] table.\n\n\
         verified-download <URL> <OUTPUT> <EXPECTED-SHA256>\n      Download an HTTPS artifact with retries and atomically verify SHA-256.\n\n\
         verify-source-revision\n      Require a clean Git working tree and print the exact HEAD revision.\n\n\
         bench [--renderer PATH] [--runs N] [--warmup-runs N]\n        [--budget-file PATH] [--output PATH] [--dry-run]\n      Benchmark the canonical GPU regression renderer and enforce its budget.\n\n\
         verify-android-16kb <APK> [--objdump PATH] [--zipalign PATH]\n      Verify 16 KB ELF LOAD alignment and APK zip alignment."
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

fn pass(message: impl fmt::Display) {
    println!("{GREEN}{BOLD}PASS{RESET} {message}");
}

fn info(message: impl fmt::Display) {
    println!("{CYAN}{BOLD}INFO{RESET} {message}");
}

fn warn(message: impl fmt::Display) {
    println!("{YELLOW}{BOLD}WARN{RESET} {message}");
}

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

fn parse_positive_usize(value: &OsStr, option: &str) -> Result<usize> {
    let parsed = value
        .to_string_lossy()
        .parse::<usize>()
        .map_err(|_| XtaskError::usage(format!("{option} must be a positive integer")))?;
    if parsed == 0 {
        return Err(XtaskError::usage(format!(
            "{option} must be a positive integer"
        )));
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Copy)]
enum MetadataFormat {
    Json,
    Shell,
}

fn parse_print_metadata_args(args: Vec<OsString>) -> Result<MetadataFormat> {
    let mut format = MetadataFormat::Json;
    let mut index = 0usize;
    while index < args.len() {
        let argument = args[index].to_string_lossy();
        let value = if argument == "--format" {
            next_value(&args, &mut index, "--format")?
                .to_string_lossy()
                .into_owned()
        } else if let Some(value) = argument.strip_prefix("--format=") {
            value.to_owned()
        } else {
            return Err(XtaskError::usage(format!(
                "unknown print-metadata option: {argument}"
            )));
        };
        format = match value.as_str() {
            "json" => MetadataFormat::Json,
            "shell" => MetadataFormat::Shell,
            _ => {
                return Err(XtaskError::usage(
                    "--format must be either json or shell",
                ))
            }
        };
        index += 1;
    }
    Ok(format)
}

#[derive(Debug)]
struct VerifiedDownloadArgs {
    url: String,
    output: PathBuf,
    expected_sha256: String,
}

fn parse_verified_download_args(args: Vec<OsString>) -> Result<VerifiedDownloadArgs> {
    if args.len() != 3 {
        return Err(XtaskError::usage(
            "verified-download requires <url> <output-path> <expected-sha256>",
        ));
    }
    let url = args[0]
        .to_str()
        .ok_or_else(|| XtaskError::usage("download URL must be valid UTF-8"))?
        .to_owned();
    let expected_sha256 = args[2]
        .to_str()
        .ok_or_else(|| XtaskError::usage("expected SHA-256 must be valid UTF-8"))?
        .to_owned();
    Ok(VerifiedDownloadArgs {
        url,
        output: PathBuf::from(args[1].clone()),
        expected_sha256,
    })
}

#[derive(Debug)]
struct BenchArgs {
    renderer: PathBuf,
    measured_runs: Option<usize>,
    warmup_runs: Option<usize>,
    budget_file: PathBuf,
    output: PathBuf,
    dry_run: bool,
}

fn parse_bench_args(args: Vec<OsString>) -> Result<BenchArgs> {
    let mut parsed = BenchArgs {
        renderer: PathBuf::from("target/release/auraw-regression-render"),
        measured_runs: None,
        warmup_runs: None,
        budget_file: PathBuf::from("benchmarks/gpu-budget.json"),
        output: PathBuf::from("target/benchmark-report.json"),
        dry_run: false,
    };

    let mut index = 0;
    while index < args.len() {
        match args[index].to_string_lossy().as_ref() {
            "--renderer" => {
                parsed.renderer = PathBuf::from(next_value(&args, &mut index, "--renderer")?)
            }
            "--runs" => {
                let value = next_value(&args, &mut index, "--runs")?;
                parsed.measured_runs = Some(parse_positive_usize(&value, "--runs")?);
            }
            "--warmup-runs" => {
                let value = next_value(&args, &mut index, "--warmup-runs")?;
                parsed.warmup_runs = Some(parse_positive_usize(&value, "--warmup-runs")?);
            }
            "--budget-file" => {
                parsed.budget_file =
                    PathBuf::from(next_value(&args, &mut index, "--budget-file")?)
            }
            "--output" => {
                parsed.output = PathBuf::from(next_value(&args, &mut index, "--output")?)
            }
            "--dry-run" => parsed.dry_run = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            unknown => {
                return Err(XtaskError::usage(format!(
                    "unknown bench option: {unknown}"
                )))
            }
        }
        index += 1;
    }
    Ok(parsed)
}

#[derive(Debug)]
struct AndroidArgs {
    apk: PathBuf,
    objdump: Option<PathBuf>,
    zipalign: Option<PathBuf>,
}

fn parse_android_args(args: Vec<OsString>) -> Result<AndroidArgs> {
    let mut apk = None;
    let mut objdump = None;
    let mut zipalign = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].to_string_lossy().as_ref() {
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
        apk: apk.ok_or_else(|| {
            XtaskError::usage("verify-android-16kb requires an APK path")
        })?,
        objdump,
        zipalign,
    })
}

fn command_check_all() -> Result<()> {
    let root = workspace_root();
    let mut failure_count = 0usize;

    info("checking source tree reachability, shaders, and generated binaries");
    let mut source_errors = validate_source_reachability(&root)?;
    source_errors.extend(validate_shader_imports(&root)?);
    source_errors.extend(validate_generated_binaries(&root)?);
    source_errors.sort();
    source_errors.dedup();
    failure_count += report_validation("source tree", &source_errors);

    info("checking immutable workflow action pins");
    let workflow_errors = validate_workflow_pins(&root)?;
    failure_count += report_validation("workflow pins", &workflow_errors);

    info("checking Gradle wrapper integrity");
    let gradle_errors = validate_gradle_wrapper(&root)?;
    failure_count += report_validation("Gradle wrapper", &gradle_errors);

    if failure_count == 0 {
        pass("all developer and CI checks completed");
        Ok(())
    } else {
        Err(XtaskError::new(format!(
            "check-all found {failure_count} error(s)"
        )))
    }
}

fn report_validation(name: &str, errors: &[String]) -> usize {
    if errors.is_empty() {
        pass(name);
        return 0;
    }

    eprintln!("{RED}{BOLD}FAIL{RESET} {name}");
    for error in errors {
        eprintln!("{RED}  -{RESET} {error}");
    }
    errors.len()
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

#[derive(Debug)]
struct Budget {
    scenes: Vec<String>,
    warmup_runs: usize,
    measured_runs: usize,
    preview_p95_ms: f64,
    export_mp_per_second_min: f64,
    startup_shader_compile_p95_ms: f64,
}

fn read_budget(path: &Path) -> Result<Budget> {
    let bytes = fs::read(path).map_err(|error| {
        XtaskError::new(format!("cannot read benchmark budget {}: {error}", path.display()))
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        XtaskError::new(format!("invalid benchmark budget {}: {error}", path.display()))
    })?;

    let schema = required_u64(&value, "schema")?;
    if schema != 1 {
        return Err(XtaskError::new(format!(
            "unsupported benchmark budget schema {schema}; expected 1"
        )));
    }
    let scenes = value
        .get("scenes")
        .and_then(Value::as_array)
        .ok_or_else(|| XtaskError::new("benchmark budget scenes must be an array"))?
        .iter()
        .map(|scene| {
            scene
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| XtaskError::new("benchmark scene names must be strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    let budgets = value
        .get("budgets")
        .ok_or_else(|| XtaskError::new("benchmark budget is missing budgets"))?;

    let budget = Budget {
        scenes,
        warmup_runs: usize::try_from(required_u64(&value, "warmup_runs")?)
            .map_err(|_| XtaskError::new("warmup_runs is too large"))?,
        measured_runs: usize::try_from(required_u64(&value, "measured_runs")?)
            .map_err(|_| XtaskError::new("measured_runs is too large"))?,
        preview_p95_ms: required_f64(budgets, "preview_p95_ms")?,
        export_mp_per_second_min: required_f64(budgets, "export_mp_per_second_min")?,
        startup_shader_compile_p95_ms: required_f64(
            budgets,
            "startup_shader_compile_p95_ms",
        )?,
    };

    if budget.warmup_runs == 0 || budget.measured_runs == 0 {
        return Err(XtaskError::new(
            "benchmark warmup_runs and measured_runs must be positive",
        ));
    }
    if budget.preview_p95_ms <= 0.0
        || budget.export_mp_per_second_min <= 0.0
        || budget.startup_shader_compile_p95_ms <= 0.0
    {
        return Err(XtaskError::new("benchmark budget values must be positive"));
    }
    Ok(budget)
}

fn required_u64(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| XtaskError::new(format!("benchmark budget {key} must be an integer")))
}

fn required_f64(value: &Value, key: &str) -> Result<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| XtaskError::new(format!("benchmark budget {key} must be a number")))
}

#[derive(Debug)]
struct SceneReport {
    name: String,
    width: u32,
    height: u32,
    megapixels: f64,
    warmup_ms: Vec<f64>,
    times_ms: Vec<f64>,
    median_ms: f64,
    p95_ms: f64,
    median_mp_per_second: f64,
    p95_mp_per_second: f64,
    latency_pass: bool,
    throughput_pass: bool,
}

fn command_bench(args: BenchArgs) -> Result<()> {
    let root = workspace_root();
    let renderer = rooted(&root, args.renderer);
    let budget_file = rooted(&root, args.budget_file);
    let output_path = rooted(&root, args.output);
    let budget = read_budget(&budget_file)?;
    let measured_runs = args.measured_runs.unwrap_or(budget.measured_runs);
    let warmup_runs = args.warmup_runs.unwrap_or(budget.warmup_runs);

    let supported: BTreeSet<&str> = BENCHMARK_SCENES.iter().map(|scene| scene.0).collect();
    let requested: BTreeSet<&str> = budget.scenes.iter().map(String::as_str).collect();
    if requested != supported {
        return Err(XtaskError::new(format!(
            "budget scenes do not match the canonical benchmark set: expected {:?}, found {:?}",
            supported, requested
        )));
    }

    let mut scene_inputs = BTreeMap::new();
    for (name, filename, width, height) in BENCHMARK_SCENES {
        let input = root.join("regression/raw").join(filename);
        if !input.is_file() {
            return Err(XtaskError::usage(format!(
                "committed benchmark scene is missing: {}",
                input.display()
            )));
        }
        scene_inputs.insert(name, (input, width, height));
    }

    if args.dry_run {
        for (name, (input, _, _)) in &scene_inputs {
            let target = root
                .join("target/benchmarks")
                .join(format!("{name}-measured-1.npz"));
            println!(
                "{}",
                display_command(
                    &renderer,
                    [
                        OsStr::new("--backend"),
                        OsStr::new("gpu"),
                        OsStr::new("--input"),
                        input.as_os_str(),
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
            "renderer does not exist: {} (build it with `cargo build --release --bin auraw-regression-render`)",
            renderer.display()
        )));
    }

    let benchmark_directory = root.join("target/benchmarks");
    fs::create_dir_all(&benchmark_directory)?;
    let mut reports = Vec::new();
    let mut startup_samples = Vec::new();

    for (name, (input, width, height)) in &scene_inputs {
        info(format!(
            "benchmarking {name} ({warmup_runs} warmup, {measured_runs} measured)"
        ));
        let mut warmup_ms = Vec::with_capacity(warmup_runs);
        let mut times_ms = Vec::with_capacity(measured_runs);

        for run in 0..warmup_runs {
            let target = benchmark_directory.join(format!("{name}-warmup-{}.npz", run + 1));
            let elapsed = run_renderer(&renderer, input, &target)?;
            warmup_ms.push(elapsed);
            startup_samples.push(elapsed);
        }
        for run in 0..measured_runs {
            let target = benchmark_directory.join(format!("{name}-measured-{}.npz", run + 1));
            times_ms.push(run_renderer(&renderer, input, &target)?);
        }

        let megapixels = f64::from(*width) * f64::from(*height) / 1_000_000.0;
        let median_ms = median(&times_ms);
        let p95_ms = percentile_95(&times_ms);
        let median_mp_per_second = megapixels / (median_ms / 1_000.0);
        let p95_mp_per_second = megapixels / (p95_ms / 1_000.0);
        let latency_pass = p95_ms <= budget.preview_p95_ms;
        let throughput_pass = median_mp_per_second >= budget.export_mp_per_second_min;

        let status = if latency_pass && throughput_pass {
            format!("{GREEN}{BOLD}PASS{RESET}")
        } else {
            format!("{RED}{BOLD}FAIL{RESET}")
        };
        println!(
            "{status} {name}: p95 {p95_ms:.2} ms (budget ≤ {:.2}), median {:.3} MP/s (budget ≥ {:.3})",
            budget.preview_p95_ms, budget.export_mp_per_second_min, median_mp_per_second
        );

        reports.push(SceneReport {
            name: (*name).to_owned(),
            width: *width,
            height: *height,
            megapixels,
            warmup_ms,
            times_ms,
            median_ms,
            p95_ms,
            median_mp_per_second,
            p95_mp_per_second,
            latency_pass,
            throughput_pass,
        });
    }

    let startup_p95_ms = percentile_95(&startup_samples);
    let startup_pass = startup_p95_ms <= budget.startup_shader_compile_p95_ms;
    if startup_pass {
        pass(format!(
            "startup/shader compile p95 {startup_p95_ms:.2} ms (budget ≤ {:.2})",
            budget.startup_shader_compile_p95_ms
        ));
    } else {
        eprintln!(
            "{RED}{BOLD}FAIL{RESET} startup/shader compile p95 {startup_p95_ms:.2} ms (budget ≤ {:.2})",
            budget.startup_shader_compile_p95_ms
        );
    }

    let passed = startup_pass
        && reports
            .iter()
            .all(|report| report.latency_pass && report.throughput_pass);
    write_benchmark_report(
        &root,
        &output_path,
        &renderer,
        &budget_file,
        &budget,
        measured_runs,
        warmup_runs,
        startup_p95_ms,
        startup_pass,
        passed,
        &reports,
    )?;
    info(format!("wrote {}", relative_display(&root, &output_path)));

    if passed {
        pass("GPU benchmark budget");
        Ok(())
    } else {
        Err(XtaskError::new("GPU benchmark budget exceeded"))
    }
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
        return Err(XtaskError::with_code(
            format!(
                "renderer failed for {} after {elapsed_ms:.2} ms",
                input.display()
            ),
            status.code().unwrap_or(1),
        ));
    }
    Ok(elapsed_ms)
}

#[allow(clippy::too_many_arguments)]
fn write_benchmark_report(
    root: &Path,
    output_path: &Path,
    renderer: &Path,
    budget_file: &Path,
    budget: &Budget,
    measured_runs: usize,
    warmup_runs: usize,
    startup_p95_ms: f64,
    startup_pass: bool,
    passed: bool,
    reports: &[SceneReport],
) -> Result<()> {
    let scenes = reports
        .iter()
        .map(|report| {
            (
                report.name.clone(),
                json!({
                    "width": report.width,
                    "height": report.height,
                    "megapixels": report.megapixels,
                    "warmup_ms": &report.warmup_ms,
                    "times_ms": &report.times_ms,
                    "median_ms": report.median_ms,
                    "p95_ms": report.p95_ms,
                    "median_megapixels_per_second": report.median_mp_per_second,
                    "p95_megapixels_per_second": report.p95_mp_per_second,
                    "latency_pass": report.latency_pass,
                    "throughput_pass": report.throughput_pass,
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();

    let report = json!({
        "schema": 3,
        "renderer": relative_display(root, renderer),
        "measured_runs": measured_runs,
        "warmup_runs": warmup_runs,
        "scenes": scenes,
        "budget": {
            "budget_file": relative_display(root, budget_file),
            "preview_p95_ms_max": budget.preview_p95_ms,
            "export_mp_per_second_min": budget.export_mp_per_second_min,
            "startup_shader_compile_p95_ms_max": budget.startup_shader_compile_p95_ms,
            "startup_shader_compile_p95_ms_measured": startup_p95_ms,
            "startup_pass": startup_pass,
            "passed": passed,
        },
        "measurement_scope": "wall-clock process startup plus canonical GPU render/readback; use native GPU timestamp queries for per-pass diagnosis",
    });

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(output_path)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    Ok(())
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
    let root = workspace_root();
    let apk = rooted(&root, args.apk);
    if !apk.is_file() {
        return Err(XtaskError::usage(format!("APK not found: {}", apk.display())));
    }

    let contract_path = root.join("Cargo.toml");
    let contract = read_workspace_metadata(&contract_path)?;
    validate_required_workspace_metadata(&contract_path, &contract)?;
    let ndk_version = metadata_string(&contract_path, &contract, "android_ndk_version")?;
    let build_tools_version =
        metadata_string(&contract_path, &contract, "android_build_tools_version")?;

    let sdk = android_sdk_root();
    let objdump = match args
        .objdump
        .or_else(|| env::var_os("LLVM_OBJDUMP").map(PathBuf::from))
    {
        Some(path) => path,
        None => {
            let sdk = sdk.as_deref().ok_or_else(|| {
                XtaskError::new(
                    "Android SDK not found; set ANDROID_SDK_ROOT/ANDROID_HOME or --objdump",
                )
            })?;
            find_llvm_objdump(sdk, ndk_version)?
        }
    };
    let zipalign = match args
        .zipalign
        .or_else(|| env::var_os("ZIPALIGN").map(PathBuf::from))
    {
        Some(path) => path,
        None => {
            let sdk = sdk.as_deref().ok_or_else(|| {
                XtaskError::new(
                    "Android SDK not found; set ANDROID_SDK_ROOT/ANDROID_HOME or --zipalign",
                )
            })?;
            sdk.join("build-tools")
                .join(build_tools_version)
                .join(executable_name("zipalign"))
        }
    };
    require_tool(&objdump, "llvm-objdump")?;
    require_tool(&zipalign, "zipalign")?;

    let temporary = TemporaryDirectory::new("auraw-16kb")?;
    let libraries = extract_64_bit_libraries(&apk, temporary.path())?;
    if libraries.is_empty() {
        warn("no arm64-v8a or x86_64 native libraries found; ELF check is not applicable");
    }

    for (archive_path, library) in &libraries {
        verify_elf_alignment(&objdump, archive_path, library)?;
        pass(format!("16 KB ELF aligned: {archive_path}"));
    }

    info(format!("running zipalign {}", zipalign.display()));
    let status = Command::new(&zipalign)
        .args(["-c", "-P", "16", "-v", "4"])
        .arg(&apk)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            XtaskError::new(format!("could not execute {}: {error}", zipalign.display()))
        })?;
    if !status.success() {
        return Err(XtaskError::with_code(
            format!("APK 16 KB zip alignment failed: {}", apk.display()),
            status.code().unwrap_or(1),
        ));
    }

    pass(format!(
        "Android 16 KB page-size checks: {}",
        relative_display(&root, &apk)
    ));
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum MetadataKind {
    String,
    PositiveInteger,
    Boolean,
}

fn command_print_metadata(format: MetadataFormat) -> Result<()> {
    let root = workspace_root();
    let manifest = root.join("Cargo.toml");
    let metadata = read_workspace_metadata(&manifest)?;
    validate_required_workspace_metadata(&manifest, &metadata)?;

    match format {
        MetadataFormat::Json => {
            let mut stdout = io::stdout().lock();
            serde_json::to_writer_pretty(&mut stdout, &metadata)?;
            stdout.write_all(b"\n")?;
        }
        MetadataFormat::Shell => {
            for (key, value) in &metadata {
                let encoded = metadata_scalar(value).ok_or_else(|| {
                    XtaskError::new(format!(
                        "{}: [workspace.metadata].{key} cannot be represented as a shell scalar",
                        manifest.display()
                    ))
                })?;
                let environment_key = format!("AURAW_{}", key.to_ascii_uppercase());
                println!(
                    "export {environment_key}={}",
                    shell_escape(OsStr::new(&encoded))
                );
            }
        }
    }
    Ok(())
}

fn command_verified_download(args: VerifiedDownloadArgs) -> Result<()> {
    if !args.url.starts_with("https://") {
        return Err(XtaskError::usage(format!(
            "refusing non-HTTPS download: {}",
            args.url
        )));
    }
    let expected = args.expected_sha256.to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(XtaskError::usage(
            "invalid checksum: expected SHA-256 must contain exactly 64 hexadecimal digits",
        ));
    }

    let root = workspace_root();
    let output = rooted(&root, args.output);
    if output.is_file() && sha256_file(&output)? == expected {
        println!("verified cached download: {}", output.display());
        return Ok(());
    }
    let parent = output.parent().ok_or_else(|| {
        XtaskError::new(format!("download output has no parent: {}", output.display()))
    })?;
    fs::create_dir_all(parent)?;
    let file_name = output
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("download");
    let temporary = parent.join(format!(
        ".{file_name}.download.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _cleanup = TemporaryFile::new(temporary.clone());

    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--silent",
            "--show-error",
            "--retry",
            "8",
            "--retry-all-errors",
            "--retry-delay",
            "2",
            "--connect-timeout",
            "30",
            "--max-time",
            "900",
            "-o",
        ])
        .arg(&temporary)
        .arg(&args.url)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| XtaskError::new(format!("could not execute curl: {error}")))?;
    if !status.success() {
        return Err(XtaskError::with_code(
            format!("HTTPS download failed: {}", args.url),
            status.code().unwrap_or(1),
        ));
    }

    let actual = sha256_file(&temporary)?;
    if actual != expected {
        return Err(XtaskError::new(format!(
            "sha256 checksum mismatch for {}\n  expected: {expected}\n  actual:   {actual}",
            args.url
        )));
    }

    if let Ok(metadata) = fs::metadata(&output) {
        fs::set_permissions(&temporary, metadata.permissions())?;
    }
    replace_file(&temporary, &output)?;
    println!("verified download: {}", output.display());
    Ok(())
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

fn command_verify_source_revision() -> Result<()> {
    let root = workspace_root();
    let status = run_command_output(
        Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(&root),
        "git status",
    )?;
    if !status.trim().is_empty() {
        let entries = status
            .lines()
            .map(|line| format!("  {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(XtaskError::new(format!(
            "Git working tree is not clean:\n{entries}"
        )));
    }

    let revision = run_command_output(
        Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&root),
        "git rev-parse HEAD",
    )?;
    let revision = revision.trim();
    if !is_full_commit_sha(revision) {
        return Err(XtaskError::new(format!(
            "git rev-parse returned an invalid HEAD revision: {revision:?}"
        )));
    }
    println!("{revision}");
    Ok(())
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

fn metadata_string<'a>(
    path: &Path,
    values: &'a BTreeMap<String, Value>,
    key: &str,
) -> Result<&'a str> {
    values.get(key).and_then(Value::as_str).ok_or_else(|| {
        XtaskError::new(format!(
            "{}: [workspace.metadata].{key} must be a string",
            path.display()
        ))
    })
}

fn metadata_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
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

fn android_sdk_root() -> Option<PathBuf> {
    env::var_os("ANDROID_SDK_ROOT")
        .or_else(|| env::var_os("ANDROID_HOME"))
        .map(PathBuf::from)
}

fn find_llvm_objdump(sdk: &Path, expected_ndk: &str) -> Result<PathBuf> {
    let ndk = env::var_os("ANDROID_NDK_HOME")
        .or_else(|| env::var_os("ANDROID_NDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| sdk.join("ndk").join(expected_ndk));
    if !ndk.is_dir() {
        return Err(XtaskError::new(format!(
            "Android NDK {expected_ndk} not found: {}",
            ndk.display()
        )));
    }

    let prebuilt = ndk.join("toolchains/llvm/prebuilt");
    let preferred = prebuilt.join(host_tag()).join("bin").join(executable_name("llvm-objdump"));
    if preferred.is_file() {
        return Ok(preferred);
    }

    if prebuilt.is_dir() {
        for entry in fs::read_dir(&prebuilt)? {
            let candidate = entry?
                .path()
                .join("bin")
                .join(executable_name("llvm-objdump"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(XtaskError::new(format!(
        "llvm-objdump not found under {}",
        prebuilt.display()
    )))
}

fn host_tag() -> &'static str {
    match (env::consts::OS, env::consts::ARCH) {
        ("linux", _) => "linux-x86_64",
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", _) => "darwin-x86_64",
        ("windows", _) => "windows-x86_64",
        _ => "linux-x86_64",
    }
}

fn executable_name(name: &str) -> OsString {
    if cfg!(windows) {
        OsString::from(format!("{name}.exe"))
    } else {
        OsString::from(name)
    }
}

fn require_tool(path: &Path, name: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(XtaskError::new(format!("{name} not found: {}", path.display())))
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

fn verify_elf_alignment(objdump: &Path, archive_path: &str, library: &Path) -> Result<()> {
    let output = Command::new(objdump)
        .arg("-p")
        .arg(library)
        .output()
        .map_err(|error| {
            XtaskError::new(format!("could not execute {}: {error}", objdump.display()))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(XtaskError::with_code(
            if stderr.is_empty() {
                format!("llvm-objdump failed for {archive_path}")
            } else {
                format!("llvm-objdump failed for {archive_path}: {stderr}")
            },
            output.status.code().unwrap_or(1),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let load_lines: Vec<&str> = stdout.lines().filter(|line| line.contains("LOAD")).collect();
    let alignments: Vec<u32> = load_lines
        .iter()
        .filter_map(|line| parse_alignment_power(line))
        .collect();
    if alignments.is_empty() {
        return Err(XtaskError::new(format!(
            "could not read ELF LOAD alignment from {archive_path}"
        )));
    }
    if alignments.iter().any(|alignment| *alignment < 14) {
        eprintln!("{RED}under-aligned ELF LOAD segments in {archive_path}:{RESET}");
        for line in load_lines {
            eprintln!("  {line}");
        }
        return Err(XtaskError::new(format!(
            "{archive_path} has an ELF LOAD segment aligned below 2**14 bytes"
        )));
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
        hex_encode, median, native_library_path, parse_alignment_power, percentile_95, Sha256,
    };

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
    fn accepts_only_flat_apk_native_library_paths() {
        assert_eq!(
            native_library_path("lib/arm64-v8a/libauraw.so"),
            Some(("arm64-v8a", "libauraw.so"))
        );
        assert_eq!(native_library_path("assets/libauraw.so"), None);
        assert_eq!(native_library_path("lib/arm64-v8a/sub/libauraw.so"), None);
    }
}
