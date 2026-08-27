use image::{imageops::FilterType, DynamicImage, ImageFormat, Rgba, RgbaImage};
use serde_json::Value;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use zip::ZipArchive;

const ANDROID_64_BIT_ABIS: [&str; 1] = ["arm64-v8a"];
const CARGO_NDK_VERSION: &str = "4.1.2";

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
        "icons" => {
            ensure_no_extra_args(&rest, "icons")?;
            command_icons()
        }
        "build-android" => command_build_android(parse_build_android_args(rest)?),
        "build-android-libraw" => command_build_android_dependency(
            parse_build_dependency_args(rest, "build-android-libraw")?,
            AndroidDependency::LibRaw,
        ),
        "build-android-lensfun" => command_build_android_dependency(
            parse_build_dependency_args(rest, "build-android-lensfun")?,
            AndroidDependency::Lensfun,
        ),
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
        "AuRaw project-specific build helpers.\n\n\
         Usage: cargo xtask <command> [options]\n\n\
         Commands:\n\
           icons\n\
           build-android [ABI] [PROFILE]\n\
           build-android-libraw [ABI]\n\
           build-android-lensfun [ABI]\n\
           verify-android-16kb [APK] [--objdump PATH] [--zipalign PATH]"
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

#[derive(Debug)]
struct BuildAndroidArgs {
    abi: String,
    profile: String,
}

fn parse_build_android_args(args: Vec<OsString>) -> Result<BuildAndroidArgs> {
    let mut positionals = Vec::new();
    for argument in args {
        match argument.to_string_lossy().as_ref() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(XtaskError::usage(format!(
                    "unknown build-android option: {value}"
                )));
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
        abi: positionals
            .first()
            .cloned()
            .unwrap_or_else(|| "arm64-v8a".to_owned()),
        profile: positionals
            .get(1)
            .cloned()
            .unwrap_or_else(|| "release".to_owned()),
    })
}

#[derive(Debug)]
struct BuildDependencyArgs {
    abi: String,
}

fn parse_build_dependency_args(args: Vec<OsString>, command: &str) -> Result<BuildDependencyArgs> {
    let mut abi = None;
    for argument in args {
        match argument.to_string_lossy().as_ref() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(XtaskError::usage(format!(
                    "unknown {command} option: {value}"
                )));
            }
            _ if abi.is_none() => abi = Some(argument.to_string_lossy().into_owned()),
            _ => {
                return Err(XtaskError::usage(format!(
                    "{command} accepts at most one ABI positional argument"
                )));
            }
        }
    }
    Ok(BuildDependencyArgs {
        abi: abi.unwrap_or_else(|| "arm64-v8a".to_owned()),
    })
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
                )));
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
        apk: apk
            .unwrap_or_else(|| PathBuf::from("android/app/build/outputs/apk/debug/app-debug.apk")),
        objdump,
        zipalign,
    })
}

#[derive(Debug)]
struct BuildContract {
    ndk_version: String,
    build_tools_version: String,
    min_sdk: u64,
}

fn load_build_contract() -> Result<BuildContract> {
    let root = workspace_root();
    let output = Command::new("cargo")
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .map_err(|error| XtaskError::new(format!("could not execute cargo metadata: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(XtaskError::with_code(
            if stderr.is_empty() {
                "cargo metadata failed".to_owned()
            } else {
                format!("cargo metadata failed: {stderr}")
            },
            output.status.code().unwrap_or(1),
        ));
    }

    let document: Value = serde_json::from_slice(&output.stdout)?;
    let metadata = document
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| XtaskError::new("Cargo.toml is missing [workspace.metadata]"))?;
    let string = |key: &str| -> Result<String> {
        metadata
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| XtaskError::new(format!("workspace.metadata.{key} must be a string")))
    };
    let integer = |key: &str| -> Result<u64> {
        metadata
            .get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                XtaskError::new(format!(
                    "workspace.metadata.{key} must be a positive integer"
                ))
            })
    };

    Ok(BuildContract {
        ndk_version: string("android_ndk_version")?,
        build_tools_version: string("android_build_tools_version")?,
        min_sdk: integer("android_min_sdk")?,
    })
}

fn parse_properties_file(path: &Path) -> Result<Vec<(String, String)>> {
    let source = fs::read_to_string(path)
        .map_err(|error| XtaskError::new(format!("cannot read {}: {error}", path.display())))?;
    let mut values = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    Ok(values)
}

fn property_value<'a>(properties: &'a [(String, String)], key: &str) -> Option<&'a str> {
    properties
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

fn android_abi_config(abi: &str, api: u64) -> Result<(String, &'static str)> {
    match abi {
        "arm64-v8a" => Ok((
            format!("aarch64-linux-android{api}"),
            "aarch64-linux-android",
        )),
        _ => Err(XtaskError::usage(format!(
            "Unsupported ABI '{abi}' (use arm64-v8a)"
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
        Err(XtaskError::with_code(
            String::new(),
            status.code().unwrap_or(1),
        ))
    }
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
    String::from_utf8(output.stdout).map_err(|error| {
        XtaskError::new(format!("{description} produced non-UTF-8 output: {error}"))
    })
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
    let mut directories = vec![path.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new(extension))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn android_sdk_root_with_local_properties(root: &Path) -> Result<Option<PathBuf>> {
    if let Some(configured) =
        env::var_os("ANDROID_SDK_ROOT").or_else(|| env::var_os("ANDROID_HOME"))
    {
        return Ok(Some(rooted(root, configured)));
    }
    let local_properties = root.join("android/local.properties");
    if !local_properties.is_file() {
        return Ok(None);
    }
    let properties = parse_properties_file(&local_properties)?;
    Ok(property_value(&properties, "sdk.dir").map(|value| rooted(root, value)))
}

fn android_ndk_root(
    root: &Path,
    expected_version: &str,
    require_toolchain: bool,
) -> Result<PathBuf> {
    let sdk = android_sdk_root_with_local_properties(root)?;
    let configured = env::var_os("ANDROID_NDK_HOME").or_else(|| env::var_os("ANDROID_NDK_ROOT"));
    let ndk = configured
        .map(|path| rooted(root, path))
        .or_else(|| sdk.map(|sdk| sdk.join("ndk").join(expected_version)))
        .ok_or_else(|| {
            XtaskError::new("Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT).")
        })?;
    if require_toolchain && !ndk.join("build/cmake/android.toolchain.cmake").is_file() {
        return Err(XtaskError::new(format!(
            "Android NDK toolchain not found at {}",
            ndk.display()
        )));
    }
    let source_properties = ndk.join("source.properties");
    if !source_properties.is_file() {
        return Err(XtaskError::new(format!(
            "Android NDK not found at {}",
            ndk.display()
        )));
    }
    let properties = parse_properties_file(&source_properties)?;
    let revision = property_value(&properties, "Pkg.Revision").unwrap_or("");
    if revision != expected_version {
        return Err(XtaskError::new(format!(
            "Android NDK {expected_version} is required, found {} at {}",
            if revision.is_empty() {
                "unknown"
            } else {
                revision
            },
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
        XtaskError::new(format!(
            "The selected NDK has no LLVM toolchain: {}",
            ndk.display()
        ))
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

fn run_gradle_android_native_dependencies(
    root: &Path,
    abi: &str,
    profile: &str,
    min_sdk: u64,
) -> Result<()> {
    android_abi_config(abi, min_sdk)?;
    if !matches!(profile, "debug" | "release") {
        return Err(XtaskError::usage(format!(
            "Unknown profile '{profile}' (use release or debug)"
        )));
    }
    let gradlew = root.join(if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    });
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

#[derive(Debug, Clone, Copy)]
enum AndroidDependency {
    LibRaw,
    Lensfun,
}

fn command_build_android_dependency(
    args: BuildDependencyArgs,
    dependency: AndroidDependency,
) -> Result<()> {
    let contract = load_build_contract()?;
    let root = workspace_root();
    run_gradle_android_native_dependencies(&root, &args.abi, "release", contract.min_sdk)?;
    match dependency {
        AndroidDependency::LibRaw => {
            let staged = root.join("android/native/libraw").join(&args.abi);
            require_file(&staged.join("include/libraw/libraw.h"))?;
            require_file(&staged.join("lib/libraw.a"))?;
            println!(
                "AGP/CMake staged LibRaw for {} in {}",
                args.abi,
                staged.display()
            );
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
            println!(
                "AGP/CMake staged Lensfun for {} in {}",
                args.abi,
                staged.display()
            );
        }
    }
    Ok(())
}

fn command_build_android(args: BuildAndroidArgs) -> Result<()> {
    let contract = load_build_contract()?;
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
    let version_output = run_command_output(
        Command::new(&cargo).args(["ndk", "--version"]),
        "cargo ndk --version",
    )?;
    let cargo_ndk_version = version_output
        .trim()
        .strip_prefix("cargo-ndk ")
        .unwrap_or(version_output.trim());
    if cargo_ndk_version != CARGO_NDK_VERSION {
        return Err(XtaskError::new(format!(
            "cargo-ndk {CARGO_NDK_VERSION} is required, found {}",
            if cargo_ndk_version.is_empty() {
                "unknown"
            } else {
                cargo_ndk_version
            }
        )));
    }

    if env::var_os("AURAW_NATIVE_DEPS_READY").as_deref() != Some(OsStr::new("1")) {
        run_gradle_android_native_dependencies(&root, &args.abi, &args.profile, contract.min_sdk)?;
    }

    let libraw_root = root.join("android/native/libraw").join(&args.abi);
    let lensfun_root = root.join("android/native/lensfun").join(&args.abi);
    let jni_root = root.join("android/app/src/main/jniLibs");
    let abi_jni = jni_root.join(&args.abi);
    remove_path(&abi_jni)?;

    let mut command = Command::new(&cargo);
    command
        .current_dir(&root)
        .env("ANDROID_NDK_HOME", &ndk)
        .env(
            "BINDGEN_EXTRA_CLANG_ARGS",
            format!("--target={clang_target} --sysroot={}", sysroot.display()),
        )
        .env("AURAW_LIBRAW_ROOT", &libraw_root)
        .env("AURAW_LENSFUN_ROOT", &lensfun_root)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TARGET_DIR", root.join("target"))
        .args(["ndk", "-t"])
        .arg(&args.abi)
        .arg("-o")
        .arg(&jni_root)
        .args(["build", "--locked"]);

    if env::var_os("LIBCLANG_PATH").is_none() {
        if let Some(libclang) = find_host_libclang(&ndk_host) {
            command.env("LIBCLANG_PATH", libclang);
        }
    }
    for key in [
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
    ] {
        command.env_remove(key);
    }
    if args.profile == "release" {
        let source_date_epoch = run_command_output(
            Command::new("git")
                .current_dir(&root)
                .args(["show", "-s", "--format=%ct", "HEAD"]),
            "git show source date",
        )?;
        command
            .arg("--release")
            .env("AURAW_REQUIRE_COMMITTED_SOURCE", "1")
            .env("SOURCE_DATE_EPOCH", source_date_epoch.trim());
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
    println!(
        "Rust, LibRaw, and Lensfun Android libraries are ready for Gradle ({}, {}).",
        args.abi, args.profile
    );
    Ok(())
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
        if ((yi > y) != (yj > y)) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
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
    DynamicImage::ImageRgba8(render_icon(1024))
        .save_with_format(output.join("auraw-1024.png"), ImageFormat::Png)
        .map_err(|error| XtaskError::new(format!("cannot write auraw-1024.png: {error}")))?;
    DynamicImage::ImageRgba8(render_icon(256))
        .save_with_format(output.join("auraw-256.png"), ImageFormat::Png)
        .map_err(|error| XtaskError::new(format!("cannot write auraw-256.png: {error}")))?;
    write_ico(&output.join("auraw.ico"))?;
    Ok(())
}

fn command_verify_android_16kb(args: AndroidArgs) -> Result<()> {
    let contract = load_build_contract()?;
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
        return Err(XtaskError::new(format!(
            "llvm-objdump not found: {}",
            objdump.display()
        )));
    }
    if !zipalign.is_file() {
        return Err(XtaskError::new(format!(
            "zipalign {} not found: {}",
            contract.build_tools_version,
            zipalign.display()
        )));
    }

    let temporary = temporary_directory("auraw-16kb")?;
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
    if root != "lib"
        || parts.next().is_some()
        || !file.ends_with(".so")
        || !is_simple_file_name(abi)
        || !is_simple_file_name(file)
    {
        return None;
    }
    Some((abi, file))
}

fn is_simple_file_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && value != "."
        && value != ".."
}

fn verify_elf_alignment(objdump: &Path, _archive_path: &str, library: &Path) -> Result<()> {
    let output = Command::new(objdump)
        .arg("-p")
        .arg(library)
        .output()
        .map_err(|error| {
            XtaskError::new(format!("could not execute {}: {error}", objdump.display()))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let load_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("LOAD"))
        .collect();
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

fn temporary_directory(prefix: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(&format!("{prefix}-"))
        .tempdir()
        .map_err(|error| XtaskError::new(format!("cannot create temporary directory: {error}")))
}
