# Android

## Requirements

- Rust targets `aarch64-linux-android` and `x86_64-linux-android`
- JDK 17, Python 3, `libclang`, `make`, and `pkg-config`
- Android SDK, Build Tools, and NDK versions from `Cargo.toml`
- CMake 3.22.1 with Ninja and `cargo-ndk` 4.1.2

```sh
export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
eval "$(cargo xtask print-metadata --format shell)"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/$AURAW_ANDROID_NDK_VERSION"
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
```

Set `LIBCLANG_PATH` when `libclang.so` is outside the system search path.

## Build

```sh
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

Gradle builds pinned LibRaw and Lensfun dependencies and packages the APK at
`android/app/build/outputs/apk/debug/app-debug.apk`. To build one ABI through
the compatibility helper, use `cargo xtask build-android arm64-v8a release`.

The app supports 16 KB pages. Verify a built APK with:

```sh
cargo xtask verify-android-16kb android/app/build/outputs/apk/debug/app-debug.apk
```

## Storage

Imports use Android's document picker and are copied to
`Android/media/de.duecki.auraw/.library`; matching `.auraw` sidecars remain next
to each RAW. Legacy layouts are migrated only after a successful copy. Exports
are published to `Pictures/AuRaw` through MediaStore where available.


Long-running operations require the app to remain open. Android 13 and newer
may request notification permission for progress updates.
