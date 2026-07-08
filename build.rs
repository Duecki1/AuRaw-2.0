use std::env;
use std::path::PathBuf;

fn main() {
    // Locate libraw via pkg-config (respects PKG_CONFIG_PATH)
    let libraw = pkg_config::Config::new()
        .probe("libraw")
        .expect("Failed to find libraw via pkg-config. Ensure libraw is installed and PKG_CONFIG_PATH is set.");

    // Locate the actual system libraw.h within the paths returned by pkg-config
    let header_path = libraw.include_paths.iter()
        .flat_map(|p| {
            vec![
                p.join("libraw.h"),
                p.join("libraw/libraw.h"),
                p.join("libraw").join("libraw.h"),
            ]
        })
        .find(|p| p.exists())
        .expect("Could not locate libraw.h in the pkg-config include directories.");

    // Generate Rust bindings directly from the system libraw.h
    let bindings = bindgen::Builder::default()
        .header(header_path.to_string_lossy())
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
}