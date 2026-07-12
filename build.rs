use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    configure_source_revision();

    for shader in [
        "src/shaders/common.wgsl",
        "src/shaders/profile.wgsl",
        "src/shaders/raw_sampling.wgsl",
        "src/shaders/color.wgsl",
        "src/shaders/highlights.wgsl",
        "src/shaders/highlight_lch_pass.wgsl",
        "src/shaders/basic_adjustments.wgsl",
        "src/shaders/tone_common.wgsl",
        "src/shaders/tone_analysis.wgsl",
        "src/shaders/tonemap.wgsl",
        "src/shaders/adjustments.wgsl",
        "src/shaders/pass1.wgsl",
        "src/shaders/pass2.wgsl",
        "src/shaders/pass3.wgsl",
        "src/shaders/pass4.wgsl",
        "src/shaders/xtrans_pass1.wgsl",
        "src/shaders/xtrans_pass2.wgsl",
        "src/shaders/xtrans_pass3.wgsl",
        "src/shaders/xtrans_candidate_common.wgsl",
        "src/shaders/xtrans_pass4.wgsl",
        "src/shaders/xtrans_pass5.wgsl",
        "src/shaders/xtrans_pass6.wgsl",
        "src/shaders/xtrans_pass7.wgsl",
    ] {
        println!("cargo:rerun-if-changed={shader}");
    }
    for variable in [
        "PKG_CONFIG_PATH",
        "LIBRAW_NO_PKG_CONFIG",
        "AURAW_LIBRAW_ROOT",
        "AURAW_ALLOW_NO_LIBRAW",
        "BINDGEN_EXTRA_CLANG_ARGS",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rustc-check-cfg=cfg(libraw_available)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "android" {
        configure_android_libraw();
    } else {
        configure_desktop_libraw();
    }
}

fn configure_source_revision() {
    for path in ["Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for variable in ["AURAW_REQUIRE_COMMITTED_SOURCE", "AURAW_SOURCE_REVISION"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let git_revision = command_output(
        Command::new("git")
            .current_dir(&manifest_dir)
            .args(["rev-parse", "--verify", "HEAD"]),
    );
    watch_git_revision(&manifest_dir);

    let configured_revision = std::env::var("AURAW_SOURCE_REVISION").ok();
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let require_committed = release
        || std::env::var("AURAW_REQUIRE_COMMITTED_SOURCE").as_deref() == Ok("1");

    if require_committed {
        let revision = git_revision.as_deref().unwrap_or_else(|| {
            panic!("release builds must run from a Git checkout with a committed revision")
        });
        let status = command_output(
            Command::new("git")
                .current_dir(&manifest_dir)
                .args(["status", "--porcelain=v1", "--untracked-files=all"]),
        )
        .unwrap_or_else(|| panic!("could not inspect the Git source tree"));
        if !status.is_empty() {
            panic!("release builds require a clean source tree:\n{status}");
        }
        if let Some(configured) = configured_revision.as_deref() {
            assert_eq!(
                configured, revision,
                "AURAW_SOURCE_REVISION does not match the checked-out commit"
            );
        }
    }

    let revision = configured_revision
        .or(git_revision)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=AURAW_SOURCE_REVISION={revision}");
}

fn watch_git_revision(manifest_dir: &Path) {
    for git_path in ["HEAD", "packed-refs"] {
        if let Some(path) = command_output(
            Command::new("git")
                .current_dir(manifest_dir)
                .args(["rev-parse", "--git-path", git_path]),
        ) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    if let Some(reference) = command_output(
        Command::new("git")
            .current_dir(manifest_dir)
            .args(["symbolic-ref", "-q", "HEAD"]),
    ) {
        if let Some(path) = command_output(
            Command::new("git")
                .current_dir(manifest_dir)
                .args(["rev-parse", "--git-path", &reference]),
        ) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

fn command_output(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn configure_android_libraw() {
    let target = std::env::var("TARGET").expect("Cargo did not set TARGET");
    let abi =
        android_abi(&target).unwrap_or_else(|| panic!("unsupported Android target: {target}"));
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = std::env::var_os("AURAW_LIBRAW_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("android/native/libraw").join(abi));
    let header = root.join("include/libraw/libraw.h");
    let library = root.join("lib/libraw.a");

    if !header.is_file() || !library.is_file() {
        if std::env::var_os("AURAW_ALLOW_NO_LIBRAW").is_some() {
            println!(
                "cargo:warning=Android LibRaw is absent at {}; RAW loading is disabled for this check",
                root.display()
            );
            return;
        }
        panic!(
            "Android LibRaw is not built at {}. Run `scripts/build-android-libraw.sh {abi}` first, or use `scripts/build-android.sh {abi}` to build the complete APK native library.",
            root.display()
        );
    }

    println!(
        "cargo:rustc-link-search=native={}",
        root.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=raw");
    println!("cargo:rustc-link-lib=c++_shared");
    println!("cargo:rustc-link-lib=z");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=log");
    println!("cargo:rustc-link-lib=android");

    generate_bindings(&header, &[root.join("include")]);
}

fn configure_desktop_libraw() {
    let libraw = pkg_config::Config::new()
        .probe("libraw")
        .or_else(|_| pkg_config::Config::new().probe("libraw_r"));

    let Ok(libraw) = libraw else {
        println!("cargo:warning=LibRaw was not found through pkg-config; building with a disabled RAW loader. Install libraw/libraw.pc on the target machine to enable RAW decoding.");
        return;
    };

    let Some(header) = find_libraw_header(&libraw.include_paths) else {
        println!("cargo:warning=LibRaw pkg-config entry was found, but libraw.h was not in the reported include paths; building with a disabled RAW loader.");
        return;
    };

    generate_bindings(&header, &libraw.include_paths);
}

fn find_libraw_header(include_paths: &[PathBuf]) -> Option<PathBuf> {
    include_paths
        .iter()
        .flat_map(|path| [path.join("libraw.h"), path.join("libraw/libraw.h")])
        .find(|path| path.exists())
}

fn generate_bindings(header: &Path, include_paths: &[PathBuf]) {
    let mut builder = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .clang_args(
            include_paths
                .iter()
                .map(|path| format!("-I{}", path.to_string_lossy())),
        )
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        let api = std::env::var("AURAW_MIN_SDK").unwrap_or_else(|_| "26".to_owned());
        builder = builder.clang_arg(format!("-D__ANDROID_MIN_SDK_VERSION__={api}"));
    }

    let bindings = builder
        .generate()
        .expect("Unable to generate LibRaw bindings");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Couldn't write LibRaw bindings");
    println!("cargo:rustc-cfg=libraw_available");
}

fn android_abi(target: &str) -> Option<&'static str> {
    match target {
        "aarch64-linux-android" => Some("arm64-v8a"),
        "armv7-linux-androideabi" => Some("armeabi-v7a"),
        "i686-linux-android" => Some("x86"),
        "x86_64-linux-android" => Some("x86_64"),
        _ => None,
    }
}
