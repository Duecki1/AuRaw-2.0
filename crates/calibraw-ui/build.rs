use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/calibraw.ico");
    println!("cargo:rerun-if-changed=../../packaging/windows/calibraw.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        panic!("CalibRaw's Windows icon embedding currently supports the packaged GNU target");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let resource_dir = manifest_dir.join("../../packaging/windows");
    let output = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("calibraw-icon.o");
    let status = Command::new("windres")
        .current_dir(&resource_dir)
        .args([
            "--input",
            "calibraw.rc",
            "--output-format",
            "coff",
            "--output",
        ])
        .arg(&output)
        .status()
        .unwrap_or_else(|error| panic!("could not start windres: {error}"));
    assert!(
        status.success(),
        "windres failed while embedding the CalibRaw icon"
    );
    println!("cargo:rustc-link-arg-bin=calibraw={}", output.display());
}
