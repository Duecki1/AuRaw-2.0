use std::env;
use std::path::PathBuf;

fn main() {
    // Locate libraw via pkg-config (respects PKG_CONFIG_PATH)
    let libraw = pkg_config::Config::new()
        .probe("libraw")
        .expect("Failed to find libraw via pkg-config. Ensure libraw is installed and PKG_CONFIG_PATH is set.");

    // Generate Rust bindings from libraw.h
    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_args(libraw.include_paths.iter().map(|p| format!("-I{}", p.to_string_lossy())))
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings for libraw");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    // --- Ported darktable/ansel highlight reconstruction (highlights.c) ---
    // Compiled as a small static lib linked directly into the crate.
    //
    // NOTE: we deliberately do NOT pass -fopenmp here. The #pragma omp
    // lines in highlights.c are only a performance optimization (parallel
    // rows/columns) -- correctness does not depend on them, and the
    // link-time library name for OpenMP differs across toolchains
    // (libgomp on gcc/Linux, libomp on clang/macOS, vcomp on MSVC), so
    // forcing one specific link flag here would break the build on other
    // platforms. If you want multithreading later, gate an OpenMP feature
    // behind target-specific cfg and link the right runtime for each OS.
    let mut highlights_build = cc::Build::new();
    highlights_build
        .file("highlights.c")
        .flag_if_supported("-O3")
        .warnings(true);
    highlights_build.compile("auraw_highlights");

    // Rust bindings for the highlights.h surface (enum + function signature)
    let highlights_bindings = bindgen::Builder::default()
        .header("highlights.h")
        .allowlist_function("auraw_process_highlights")
        .allowlist_type("auraw_highlights_mode")
        .allowlist_var("AURAW_HIGHLIGHTS_.*")
        // Force a real Rust enum (not bindgen's default loose i32 + consts)
        // so the mode constants have a stable, predictable Rust-side name.
        .rustified_enum("auraw_highlights_mode")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings for highlights.h");

    highlights_bindings
        .write_to_file(out_path.join("highlights_bindings.rs"))
        .expect("Couldn't write highlights bindings!");

    println!("cargo:rerun-if-changed=highlights.c");
    println!("cargo:rerun-if-changed=highlights.h");
}