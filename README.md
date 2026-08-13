# AuRaw

GPU-based, non-destructive RAW editor for Linux, Windows, macOS, and Android,
built with Rust, egui, and wgpu.

## Features

- Folder and Android document-picker libraries with `.auraw` edit sidecars
- Self-hosted cloud libraries with offline caching and conflict-safe sync
- Scene-linear RAW processing, DCP profiles, Lensfun corrections, local masks,
  inpainting, AI selections, denoise, color tools, and creative effects
- PNG, JPEG, and ICC-managed TIFF export

Linux and Android are the primary targets. CI also produces Windows builds and
native Apple Silicon and Intel macOS bundles.

## Linux

Install Rust with [rustup](https://rustup.rs/) and the build dependencies:

```sh
sudo apt update
sudo apt install build-essential pkg-config libclang-dev libraw-dev \
  liblensfun-dev liblensfun-data-v1 libasound2-dev libdbus-1-dev \
  libegl1-mesa-dev libfontconfig1-dev libgl1-mesa-dev libudev-dev \
  libwayland-dev libx11-dev libxkbcommon-dev
cargo run -p auraw-ui --bin auraw --release
```

## Android

Install JDK 17, the Android SDK/NDK versions declared in `Cargo.toml`, CMake
3.22.1, `libclang`, and `cargo-ndk` 4.1.2. Then run:

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
eval "$(cargo xtask print-metadata --format shell)"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/$AURAW_ANDROID_NDK_VERSION"
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

See [ANDROID.md](ANDROID.md) for platform-specific details.

## Development

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask check-all
```

Architecture and format notes:

- [Cargo workspace](docs/CARGO_WORKSPACE.md)
- [TIFF contract](docs/TIFF.md)
- [Development](docs/DEVELOPMENT.md)

## License

AuRaw is GPL-3.0-or-later. See [COPYING](COPYING), [NOTICE.md](NOTICE.md), and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
