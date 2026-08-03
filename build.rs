use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build_support/lensfun_version.rs"]
mod lensfun_version;
#[path = "build_support/shader_preprocessor.rs"]
mod shader_preprocessor;

fn main() {
    configure_source_revision();
    generate_shader_sources();
    embed_windows_application_icon();

    for shader in [
        "src/shaders/common.wgsl",
        "src/shaders/profile.wgsl",
        "src/shaders/raw_sampling.wgsl",
        "src/shaders/noise.wgsl",
        "src/shaders/noise_ca_finish.wgsl",
        "src/shaders/color.wgsl",
        "src/shaders/color_denoise.wgsl",
        "src/shaders/highlights.wgsl",
        "src/shaders/basic_adjustments.wgsl",
        "src/shaders/tone_common.wgsl",
        "src/shaders/tone_analysis.wgsl",
        "src/shaders/tonemap.wgsl",
        "src/shaders/adjustments.wgsl",
        "src/shaders/detail_capture.wgsl",
        "src/shaders/detail_scale_space.wgsl",
        "src/shaders/regression_scene.wgsl",
        "src/shaders/pass1.wgsl",
        "src/shaders/pass2.wgsl",
        "src/shaders/pass3.wgsl",
        "src/shaders/pass4.wgsl",
        "src/shaders/dual_demosaic.wgsl",
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
        "AURAW_LENSFUN_ROOT",
        "AURAW_ALLOW_NO_LIBRAW",
        "BINDGEN_EXTRA_CLANG_ARGS",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rustc-check-cfg=cfg(libraw_available)");
    println!("cargo:rustc-check-cfg=cfg(lensfun_available)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "android" {
        configure_android_libraw();
        configure_android_lensfun();
    } else {
        configure_desktop_libraw();
        configure_desktop_lensfun();
    }
}

fn embed_windows_application_icon() {
    println!("cargo:rerun-if-changed=packaging/icons/auraw.ico");
    println!("cargo:rerun-if-changed=packaging/windows/auraw.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        panic!("AuRaw's Windows icon embedding currently supports the packaged GNU target");
    }

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set CARGO_MANIFEST_DIR: {error}")),
    );
    let resource_dir = manifest_dir.join("packaging/windows");
    let output = PathBuf::from(
        std::env::var("OUT_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set OUT_DIR: {error}")),
    )
    .join("auraw-icon.o");
    let status = Command::new("windres")
        .current_dir(&resource_dir)
        .args(["--input", "auraw.rc", "--output-format", "coff", "--output"])
        .arg(&output)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "could not start windres to embed {}: {error}",
                resource_dir.join("auraw.rc").display()
            )
        });
    assert!(
        status.success(),
        "windres failed while embedding the AuRaw icon"
    );
    println!("cargo:rustc-link-arg-bin=auraw={}", output.display());
}

fn generate_shader_sources() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set CARGO_MANIFEST_DIR: {error}")),
    );
    let shader_dir = manifest_dir.join("src/shaders");
    let output_dir = PathBuf::from(
        std::env::var("OUT_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set OUT_DIR: {error}")),
    );

    shader_preprocessor::generate_shader_sources(&shader_dir, &output_dir)
        .unwrap_or_else(|error| panic!("could not generate WGSL shader sources: {error}"));
}

fn allow_no_libraw() -> bool {
    std::env::var("AURAW_ALLOW_NO_LIBRAW").is_ok_and(|value| matches!(value.as_str(), "1" | "true"))
}

fn configure_source_revision() {
    for path in ["Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for variable in ["AURAW_REQUIRE_COMMITTED_SOURCE", "AURAW_SOURCE_REVISION"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let git_revision = command_output(Command::new("git").current_dir(&manifest_dir).args([
        "rev-parse",
        "--verify",
        "HEAD",
    ]));
    watch_git_revision(&manifest_dir);

    let configured_revision = std::env::var("AURAW_SOURCE_REVISION").ok();
    let require_committed = std::env::var("AURAW_REQUIRE_COMMITTED_SOURCE").as_deref() == Ok("1");

    if require_committed {
        let revision = git_revision.as_deref().unwrap_or_else(|| {
            panic!("reproducible builds must run from a Git checkout with a committed revision")
        });
        let status = command_output(Command::new("git").current_dir(&manifest_dir).args([
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ]))
        .unwrap_or_else(|| panic!("could not inspect the Git source tree"));
        if !status.is_empty() {
            panic!("reproducible builds require a clean source tree:\n{status}");
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
        if let Some(path) = command_output(Command::new("git").current_dir(manifest_dir).args([
            "rev-parse",
            "--git-path",
            git_path,
        ])) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    if let Some(reference) = command_output(Command::new("git").current_dir(manifest_dir).args([
        "symbolic-ref",
        "-q",
        "HEAD",
    ])) {
        if let Some(path) = command_output(Command::new("git").current_dir(manifest_dir).args([
            "rev-parse",
            "--git-path",
            &reference,
        ])) {
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
        if allow_no_libraw() {
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

fn configure_android_lensfun() {
    let target = std::env::var("TARGET").expect("Cargo did not set TARGET");
    let abi =
        android_abi(&target).unwrap_or_else(|| panic!("unsupported Android target: {target}"));
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = std::env::var_os("AURAW_LENSFUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("android/native/lensfun").join(abi));
    let header = root.join("include/lensfun/lensfun.h");
    let library = root.join("lib/liblensfun.a");

    if !header.is_file() || !library.is_file() {
        panic!(
            "Android Lensfun is not built at {}. Run `scripts/build-android-lensfun.sh {abi}` first, or use `scripts/build-android.sh {abi}` to build the complete APK native library.",
            root.display()
        );
    }

    println!(
        "cargo:rustc-link-search=native={}",
        root.join("lib").display()
    );
    for library in [
        "lensfun", "glib-2.0", "pcre2-8", "ffi", "z", "intl", "iconv", "charset",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }

    let header_source = std::fs::read_to_string(&header)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", header.display()));
    let version = lensfun_version::parse_lensfun_header_version(&header_source)
        .and_then(|version| {
            lensfun_version::validate_supported_lensfun_version(&version.to_string())?;
            Ok(version)
        })
        .unwrap_or_else(|error| panic!("{}: {error}", header.display()));
    generate_lensfun_bindings(&header, &[root.join("include")]);
    println!("cargo:rustc-env=AURAW_LENSFUN_BUILD_VERSION={version}");
    println!("cargo:rustc-cfg=lensfun_available");
}

fn configure_desktop_lensfun() {
    for variable in ["LENSFUN_NO_PKG_CONFIG", "PKG_CONFIG_PATH"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    println!("cargo:rerun-if-changed=build_support/lensfun_version.rs");

    let lensfun = match pkg_config::Config::new().probe("lensfun") {
        Ok(lensfun) => lensfun,
        Err(_) => {
            println!(
                "cargo:warning=Lensfun was not found through pkg-config; lens correction will be disabled. Install a supported Lensfun 0.3.2 through 0.3.4 development package to enable it."
            );
            return;
        }
    };

    let version = lensfun_version::validate_supported_lensfun_version(&lensfun.version)
        .unwrap_or_else(|error| panic!("{error}"));
    let header = find_lensfun_header(&lensfun.include_paths).unwrap_or_else(|| {
        panic!(
            "Lensfun {} was found, but lensfun.h was not present in the pkg-config include paths: {:?}",
            lensfun.version, lensfun.include_paths
        )
    });
    let header_source = std::fs::read_to_string(&header)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", header.display()));
    let header_version = lensfun_version::parse_lensfun_header_version(&header_source)
        .and_then(|header_version| {
            lensfun_version::validate_supported_lensfun_version(&header_version.to_string())?;
            Ok(header_version)
        })
        .unwrap_or_else(|error| panic!("{}: {error}", header.display()));
    if header_version != version {
        panic!(
            "Lensfun pkg-config reports {version}, but {} declares {header_version}; refusing to generate bindings from a mismatched ABI",
            header.display()
        );
    }
    generate_lensfun_bindings(&header, &lensfun.include_paths);
    println!("cargo:rustc-env=AURAW_LENSFUN_BUILD_VERSION={version}");
    println!("cargo:rustc-cfg=lensfun_available");
}

fn find_lensfun_header(include_paths: &[PathBuf]) -> Option<PathBuf> {
    include_paths
        .iter()
        .flat_map(|path| [path.join("lensfun.h"), path.join("lensfun/lensfun.h")])
        .find(|path| path.is_file())
}

fn generate_lensfun_bindings(header: &Path, include_paths: &[PathBuf]) {
    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .clang_args(
            include_paths
                .iter()
                .map(|path| format!("-I{}", path.to_string_lossy())),
        )
        .allowlist_function(
            "lf_(free|mlstr_get|db_new|db_destroy|db_load|db_load_file|db_find_cameras|db_find_cameras_ext|db_find_lenses_hd|db_get_lenses|modifier_new|modifier_destroy|modifier_initialize|modifier_get_auto_scale|modifier_add_coord_callback_scale|modifier_apply_subpixel_geometry_distortion|modifier_apply_color_modification)",
        )
        .allowlist_type(
            "lf(Camera|Lens|Database|Modifier|LensType|PixelFormat|ComponentRole|Error)",
        )
        .allowlist_var("LF_(NO_ERROR|SEARCH_.*|PF_F32|MODIFY_.*|CR_.*|VERSION.*)")
        .prepend_enum_name(false)
        .layout_tests(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .unwrap_or_else(|error| panic!("could not generate Lensfun bindings from {}: {error}", header.display()));

    let output_dir = std::env::var("OUT_DIR")
        .unwrap_or_else(|error| panic!("Cargo did not set OUT_DIR: {error}"));
    let output = PathBuf::from(output_dir).join("lensfun_bindings.rs");
    bindings
        .write_to_file(&output)
        .unwrap_or_else(|error| panic!("could not write {}: {error}", output.display()));
}

fn configure_desktop_libraw() {
    let target_is_macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let probe = |name| {
        let mut config = pkg_config::Config::new();
        // Homebrew LibRaw's pkg-config metadata currently contains
        // `-lstdc++`, which Apple removed in favor of libc++. Suppress the
        // automatic cargo directives on macOS so the dependency can be
        // translated below without affecting other desktop targets.
        config.cargo_metadata(!target_is_macos);
        config.probe(name)
    };
    let libraw = probe("libraw").or_else(|_| probe("libraw_r"));

    let Ok(libraw) = libraw else {
        allow_or_fail_without_desktop_libraw(
            "LibRaw was not found through pkg-config. Install libraw/libraw.pc to enable RAW decoding.",
        );
        return;
    };

    if target_is_macos {
        emit_macos_libraw_link_metadata(&libraw);
    }

    let Some(header) = find_libraw_header(&libraw.include_paths) else {
        allow_or_fail_without_desktop_libraw(
            "LibRaw pkg-config metadata was found, but libraw.h was absent from its include paths.",
        );
        return;
    };

    generate_bindings(&header, &libraw.include_paths);
}

fn emit_macos_libraw_link_metadata(libraw: &pkg_config::Library) {
    for path in &libraw.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for path in &libraw.framework_paths {
        println!("cargo:rustc-link-search=framework={}", path.display());
    }
    for library in &libraw.libs {
        let library = if library == "stdc++" { "c++" } else { library };
        println!("cargo:rustc-link-lib={library}");
    }
    for framework in &libraw.frameworks {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

fn allow_or_fail_without_desktop_libraw(reason: &str) {
    if allow_no_libraw() {
        println!(
            "cargo:warning={reason} AURAW_ALLOW_NO_LIBRAW explicitly permits this non-production check build to use a disabled RAW loader."
        );
        return;
    }

    panic!(
        "{reason} AuRaw desktop builds require LibRaw by default; set AURAW_ALLOW_NO_LIBRAW=1 only for an intentional non-production check build."
    );
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
