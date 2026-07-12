# AuRaw

GPU-based RAW image editor for Linux and Android.

## Linux (Debian/Ubuntu)

Install the build dependencies:

```sh
sudo apt update
sudo apt install build-essential pkg-config libclang-dev libraw-dev \
  libasound2-dev libdbus-1-dev libegl1-mesa-dev libfontconfig1-dev \
  libgl1-mesa-dev libudev-dev libwayland-dev libx11-dev libxkbcommon-dev
```

Install Rust with [rustup](https://rustup.rs/), then run from the repository
root:

```sh
cargo run --release
```

## Android

Install:

- JDK 17
- Gradle 8.11.1 or newer in the Gradle 8 series
- Android SDK 35, Build Tools 35.0.0, NDK 27.0.12077973, and CMake 3.22.1
- `libclang` and `cargo-ndk` 4.1.2

Set up Rust and the Android environment:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked

export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/27.0.12077973"
```

Build the APK from the repository root:

```sh
gradle assembleDebug -PaurawAbi=arm64-v8a
```

Install it on a connected device:

```sh
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

For additional Android setup details, see [ANDROID.md](ANDROID.md).
