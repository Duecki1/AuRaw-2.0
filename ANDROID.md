# Android build

AuRaw's Android app uses the same Rust UI, LibRaw loader, WGSL shaders, and
wgpu processing pipeline as the Linux app. Android-specific code is limited to
the application entry point and the system file-picker bridge.

## Prerequisites

- Rust with the Android target for the device (normally
  `aarch64-linux-android`)
- Android SDK 35 and Android NDK 27.0.12077973
- Android SDK CMake 3.22.1 (with its bundled Ninja), a JDK, host `libclang`, and Gradle 8.11.1 or newer in the Gradle 8 series (CI uses 8.11.1)
- `cargo-ndk` 4.1.2 (`cargo install cargo-ndk --version 4.1.2 --locked`)

Set `ANDROID_SDK_ROOT` and `ANDROID_NDK_HOME`, for example:

```sh
export ANDROID_SDK_ROOT="/home/duecki/Android/Sdk"
export ANDROID_NDK_HOME="/home/duecki/Android/Sdk/ndk/27.0.12077973"
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
```

On Debian/Ubuntu, the bindgen prerequisite is provided by `libclang-dev`. If
libclang is installed somewhere nonstandard, set `LIBCLANG_PATH` to the
directory containing `libclang.so`. The build script automatically uses the
NDK's copy when that NDK distribution includes one.

## Build and run

The simplest route is to open the `android` directory in Android Studio and
press Run. The variant pre-build task builds LibRaw and the matching debug or
release Rust library before Gradle packages the app.

For a command-line build:

```sh
cd android
gradle assembleDebug -PaurawAbi=arm64-v8a
```

The debug APK is written to `android/app/build/outputs/apk/debug/app-debug.apk`.
To build only the native LibRaw and Rust library without packaging an APK:

```sh
./scripts/build-android.sh arm64-v8a release
```

The first native build downloads LibRaw 0.22.1 and its official companion
CMake files by immutable commit ID, then cross-compiles a static library with
the pinned NDK. Generated sources, libraries, and APKs are ignored by Git. The native build
also copies `libc++_shared.so` from the pinned NDK instead of storing it in the
repository. No LibRaw installation on the Linux host is required. LibRaw is
cached for development builds until its version, ABI, API level, or NDK
revision changes; set `AURAW_REBUILD_LIBRAW=1` to force a clean native rebuild.
Release builds always discard the ignored native cache and rebuild it from the
pinned source revisions.

Other supported ABI names are `armeabi-v7a`, `x86`, and `x86_64`. Build and
package one ABI at a time by passing the same name to the script and the Gradle
`aurawAbi` property.

## File access and GPU behavior

The Open RAW button launches Android's Storage Access Framework with
`ACTION_OPEN_DOCUMENT`. Because document-provider URIs are not filesystem
paths, the activity copies the selected document into app-private cache
storage. Rust passes that real path to `libraw_open_file`, and deletes the
temporary copy as soon as decoding finishes. This needs no broad storage
permission and works with local files, SD-card providers, and cloud document
providers that offer a readable stream.

eframe is forced to use wgpu. wgpu includes Vulkan and GLES backends on
Android, and AuRaw requests the adapter's actual 2D texture-size limit so RAWs
wider than eframe's usual 8192-pixel default can be processed when the device
supports them. Pass 3 reuses pass 1's intermediate texture, saving eight bytes
of GPU allocation per RAW pixel (about 192 MB for a 24-megapixel file). Very
large RAW files still require substantial device GPU memory.

## Linux remains unchanged

On the computer where LibRaw is installed in `/usr/local`, continue to use:

```sh
LIBRARY_PATH=/usr/local/lib \
PKG_CONFIG_PATH=/usr/local/lib/pkgconfig \
cargo run --release
```

On a computer without desktop LibRaw, AuRaw still compiles, but the desktop RAW
loader reports that it was disabled at build time.

Release builds must run from a clean Git checkout. The full commit ID is embedded
in the Rust library, and the build scripts discard native output if the source
changes while compilation is running. See `REPRODUCIBLE_BUILDS.md`.
