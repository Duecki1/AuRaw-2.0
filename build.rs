fn main() {
    println!("cargo:rerun-if-changed=src/shaders/pipeline.wgsl");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=LIBRAW_NO_PKG_CONFIG");
    println!("cargo:rustc-check-cfg=cfg(libraw_available)");

    let libraw = pkg_config::Config::new()
        .probe("libraw")
        .or_else(|_| pkg_config::Config::new().probe("libraw_r"));

    let Ok(libraw) = libraw else {
        println!("cargo:warning=LibRaw was not found through pkg-config; building with a disabled RAW loader. Install libraw/libraw.pc on the target machine to enable RAW decoding.");
        return;
    };

    let Some(header_path) = libraw
        .include_paths
        .iter()
        .flat_map(|p| {
            [
                p.join("libraw.h"),
                p.join("libraw/libraw.h"),
                p.join("libraw").join("libraw.h"),
            ]
        })
        .find(|p| p.exists())
    else {
        println!("cargo:warning=LibRaw pkg-config entry was found, but libraw.h was not in the reported include paths; building with a disabled RAW loader.");
        return;
    };

    let bindings = bindgen::Builder::default()
        .header(header_path.to_string_lossy())
        .clang_args(
            libraw
                .include_paths
                .iter()
                .map(|p| format!("-I{}", p.to_string_lossy())),
        )
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate LibRaw bindings");

    let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write LibRaw bindings");

    println!("cargo:rustc-cfg=libraw_available");
}
