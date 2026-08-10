use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/auraw.ico");
    println!("cargo:rerun-if-changed=../../packaging/windows/auraw.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        panic!("AuRaw's Windows icon embedding currently supports the packaged GNU target");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let resource_dir = manifest_dir.join("../../packaging/windows");
    let output = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("auraw-icon.o");
    let status = Command::new("windres")
        .current_dir(&resource_dir)
        .args(["--input", "auraw.rc", "--output-format", "coff", "--output"])
        .arg(&output)
        .status()
        .unwrap_or_else(|error| panic!("could not start windres: {error}"));
    assert!(
        status.success(),
        "windres failed while embedding the AuRaw icon"
    );
    println!("cargo:rustc-link-arg-bin=auraw={}", output.display());
}
