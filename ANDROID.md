# Android

## Requirements

- Rust target `aarch64-linux-android`
- JDK 17, Python 3, `libclang`, `make`, and `pkg-config`
- Android SDK, Build Tools, and NDK versions from `Cargo.toml`
- CMake 3.22.1 with Ninja and `cargo-ndk` 4.1.2

```sh
export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
```

Set `LIBCLANG_PATH` when `libclang.so` is outside the system search path.

## Build

```sh
./gradlew assembleDebug
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

Gradle builds pinned LibRaw and Lensfun dependencies and packages the APK at
`android/app/build/outputs/apk/debug/app-debug.apk`. To build one ABI through
the compatibility helper, use `cargo xtask build-android arm64-v8a release`.

The app supports 16 KB pages. Verify a built APK with:

```sh
cargo xtask verify-android-16kb android/app/build/outputs/apk/debug/app-debug.apk
```

## GitHub release signing

Pushes to `main` and manual runs of the **Build Linux and Android** workflow
build a signed release APK. Pull requests build a debug APK because repository
secrets are not available to untrusted forks.

Create the upload keystore once and keep it backed up securely. Losing it means
future APK updates cannot be signed with the same identity.

```sh
keytool -genkeypair \
  -keystore calibraw-release.keystore \
  -storetype PKCS12 \
  -alias calibraw \
  -keyalg RSA \
  -keysize 4096 \
  -validity 10000
openssl base64 -A -in calibraw-release.keystore > calibraw-release.keystore.base64
```

In the GitHub repository, open **Settings > Secrets and variables > Actions**,
choose **New repository secret**, and add:

- `CALIBRAW_ANDROID_KEYSTORE_BASE64`: contents of
  `calibraw-release.keystore.base64`
- `CALIBRAW_ANDROID_STORE_PASSWORD`: the keystore password
- `CALIBRAW_ANDROID_KEY_ALIAS`: the alias passed to `keytool` (`calibraw` above)
- `CALIBRAW_ANDROID_KEY_PASSWORD`: the key password (use the keystore password
  for the PKCS12 keystore generated above)

The release job fails instead of creating an unsigned release when any secret
is absent or invalid. Its artifact is named
`calibraw-android-release-arm64-v8a` and contains the APK plus its SHA-256 file.

## Storage

Imports use Android's document picker and are copied to
`Android/media/de.duecki.calibraw/.library`; matching `.calibraw` sidecars remain next
to each RAW. Legacy layouts are migrated only after a successful copy. Exports
are published to `Pictures/CalibRaw` through MediaStore where available.


Long-running operations require the app to remain open. Android 13 and newer
may request notification permission for progress updates.
