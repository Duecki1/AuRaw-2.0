# Reproducible builds

AuRaw treats `Cargo.lock`, `rust-toolchain.toml`, the Android Gradle plugin,
Gradle, the Android SDK/NDK versions, and the LibRaw source revisions as build
inputs. CI uses only locked Cargo resolution and fixed toolchain versions.
The Rust compiler is pinned to 1.92.0, the minimum version required by the
locked `eframe`/`egui` 0.35 dependency graph.

## Source revision rule

Release binaries must come from a clean Git checkout. `build.rs` embeds the
full commit ID as `auraw::SOURCE_REVISION`. This applies both to the official
scripts and direct commands such as `cargo run --release`; release builds are
rejected when:

- the project is not inside a Git checkout;
- tracked or untracked source files differ from `HEAD`; or
- `AURAW_SOURCE_REVISION` does not match `HEAD`.

The official Linux and Android scripts additionally check the revision both
before and after compilation and set deterministic build environment values. If
the source changes while compiling, the generated binary is removed. Direct
Cargo release commands still require a clean committed revision, but the wrapper
scripts are the canonical artifact-producing path.

## Linux release

```sh
./scripts/build-linux.sh
```

The script accepts no Cargo overrides. It runs `cargo build --locked --release`
with incremental compilation disabled and `SOURCE_DATE_EPOCH` set to the commit
timestamp.

## Android release

The Android build is pinned to:

- Android SDK/target 35 and build-tools 35.0.0;
- Android NDK 27.0.12077973;
- CMake 3.22.1 (including its bundled Ninja);
- Gradle 8.11.1 and Android Gradle plugin 8.9.2;
- cargo-ndk 4.1.2;
- LibRaw 0.22.1 commit
  `b860248a89d9082b8e0a1e202e516f46af9adb29`;
- LibRaw-cmake commit
  `eb98e4325aef2ce85d2eb031c2ff18640ca616d3`.

Install the pinned cargo extension with:

```sh
cargo install cargo-ndk --version 4.1.2 --locked
```

Then build from a clean checkout:

```sh
./scripts/build-android.sh arm64-v8a release
```

For a release, the script removes the complete ignored Android native cache,
rebuilds LibRaw from the pinned commits, and deletes the selected ABI output
before invoking `cargo ndk`. It then regenerates both `libauraw.so` and the
pinned NDK's `libc++_shared.so`, so Gradle cannot package a stale native
library.

## CI coverage

`.github/workflows/ci.yml` has separate Linux and Android jobs. It validates the
source tree, uses locked Cargo dependencies, checks formatting, runs Rust tests,
parses and validates the composed WGSL shaders with naga, and runs clippy. It
also builds the Linux release, checks the Android Rust target, rebuilds the
Android native libraries from pinned sources, assembles an APK, and runs
Android lint without using a committed native binary.
