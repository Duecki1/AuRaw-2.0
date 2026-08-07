# Android build

AuRaw's Android app uses the same Rust UI, LibRaw loader, WGSL shaders, and
wgpu processing pipeline as the Linux app. Android-specific code is limited to
the application entry point and the system file-picker bridge.

## Prerequisites

- Rust with both Android targets used by the default build:
  `aarch64-linux-android` and `x86_64-linux-android`
- Android SDK platform and NDK versions declared in `Cargo.toml` `[workspace.metadata]`
- Android SDK CMake 3.22.1 with Ninja, a JDK, Python 3, host `libclang`,
  `make`, and `pkg-config`
- `cargo-ndk` 4.1.2 (`cargo install cargo-ndk --version 4.1.2 --locked`)

Set `ANDROID_SDK_ROOT` and `ANDROID_NDK_HOME`, for example:

```sh
export ANDROID_SDK_ROOT="/home/duecki/Android/Sdk"
eval "$(cargo xtask print-metadata --format shell)"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/$AURAW_ANDROID_NDK_VERSION"
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
```

On Debian/Ubuntu, install the host tools with `libclang-dev`, `make`,
`pkg-config`, and Python 3. If libclang is installed somewhere nonstandard, set
`LIBCLANG_PATH` to the directory containing `libclang.so`. No project-local
Python virtualenv or system Meson installation is required: CMake fetches the
pinned Meson source and invokes `meson.py` directly for GLib.

## Build and run

Open the `android` directory in Android Studio and press Run, or build from the
repository root:

```sh
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
```

This builds both supported 64-bit ABIs. The legacy single-value `aurawAbi`
property remains accepted for compatibility, but new documentation and CI use
the comma-separated `aurawAbis` contract. The
debug APK is written to `android/app/build/outputs/apk/debug/app-debug.apk`.

### Native dependency graph

`android/app/src/main/cpp/CMakeLists.txt` is registered through AGP
`externalNativeBuild`. For each active ABI, it uses the NDK toolchain to:

1. Fetch SHA-256-pinned LibRaw 0.22.1 and its pinned CMake overlay, then build
   the PIC `raw` static target with optional codecs and examples disabled.
2. Fetch SHA-256-pinned Lensfun 0.3.4 and build it with tests, lenstool,
   documentation, Python, helper scripts, and SSE-specific paths disabled.
3. Build only Lensfun's mandatory static support stack (libiconv and GLib with
   fallback PCRE2, libffi, and zlib) as CMake `ExternalProject` dependencies.
4. Stage headers and archives under `android/native/{libraw,lensfun}/<abi>`.
5. Run `cargo-ndk` after CMake and statically link those archives into
   `libauraw.so`; only `libc++_shared.so` remains a separate runtime library.

Lensfun 0.3.4 cannot be made completely GLib-free using its upstream options:
the core target itself requires GLib. The standalone switches remove optional
programs and test-only dependencies, while the minimal GLib stack above keeps
the core API intact. CMake also exposes `auraw::libraw`, `auraw::lensfun`, and
`auraw::native_static` for a future C++ JNI target. Prefab is not needed for the
current app because the final Rust JNI library consumes the static archives.

Lensfun's architecture-independent XML database is installed as APK assets and
copied to app-private storage on first launch, because Lensfun loads profiles
from filesystem paths. Generated sources, libraries, and APKs remain ignored by
Git.

To run only the native CMake stage:

```sh
./gradlew :app:externalNativeBuildDebug -PaurawAbis=arm64-v8a,x86_64
```

The compatibility CLI now delegates native dependency work to that same Gradle
task before invoking Cargo:

```sh
cargo xtask build-android arm64-v8a release
```

### 16 KB memory-page support

The project pins NDK r28 and AGP 8.9.2, disables legacy JNI packaging, and adds
explicit `-Wl,-z,max-page-size=16384` and
`-Wl,-z,common-page-size=16384` linker arguments to both CMake and Rust Android
targets. Static archives are compiled as PIC and become part of the final JNI
ELF; AGP packages the resulting uncompressed `.so` files with 16 KB zip
alignment.

After building an APK, verify both ELF LOAD alignment and APK zip alignment:

```sh
cargo xtask verify-android-16kb android/app/build/outputs/apk/debug/app-debug.apk
```

The verifier checks every 64-bit `.so` with the pinned NDK's `llvm-objdump` and
runs the pinned Build Tools `zipalign -c -P 16 -v 4`. Runtime testing should
also be performed on a 16 KB Android device or emulator; `adb shell getconf
PAGE_SIZE` must report `16384`.

## Touch preview controls

- Drag with one finger to pan the preview when no mask tool is editing it.
- Pinch with two fingers to zoom around the gesture center. Moving both fingers
  together pans at the same time.
- While editing masks, use two fingers to navigate without painting or moving the
  mask. Lift both fingers before resuming a one-finger mask edit.
- Brush input, cursor rings, and mask overlays are clipped to the visible preview;
  touching the bottom or side editor panels never paints or shows a brush cursor.
- Double-tap the preview to return to fit view.
- After roughly one second without viewport movement, AuRaw rerenders only the
  visible RAW region at the selected preview quality.
- While zoomed, adjustment sliders, tone curves, and color wheels update that
  reusable visible-region pipeline directly. The full-frame proxy is deferred
  until fit view, avoiding whole-preview renders during touch interaction.

## RAW library, file access, and GPU behavior

The Library tab's floating **+** button launches Android's Storage Access
Framework with `ACTION_OPEN_DOCUMENT`. Press and hold a thumbnail to open its
context menu without also opening the photo; Android offers Open, Reset all
adjustments, and Delete actions there. Imported RAWs are copied into one
canonical app-media folder:

`Android/media/de.duecki.auraw/.library`

The `.library` directory is intentionally hidden and contains both imported RAW
files and their `<RAW display name>.auraw` sidecars. AuRaw uses
`getExternalMediaDirs()` to reach its own `Android/media` area, so imports do not
need broad storage permission on Android 8 or newer. The Library scans only this
canonical directory for new data.

Older AuRaw builds used two different layouts: RAWs directly in the app-media
root on Android 8–9 and app-owned MediaStore Downloads rows under
`Download/AuRaw` on Android 10+. Current builds migrate those legacy entries to
`.library`. A legacy item is removed only after its RAW and available sidecar
have been copied successfully; not-yet-migrated entries remain readable as an
upgrade fallback instead of being discarded. New imports never write to the old
Downloads collection.

Embedded RAW previews are read lazily through a file descriptor and
`/proc/self/fd`. When a provider or LibRaw path cannot seek that descriptor, AuRaw
materializes a temporary cache copy and removes it after decode. This also works
with SD-card and cloud document providers offered by the system picker during
import.

Non-destructive Develop settings are stored beside the imported RAW as
`<RAW display name>.auraw` inside `.library`. Saves are written to a temporary
sibling first and then atomically replace the previous sidecar where the
filesystem supports atomic moves, so readers do not observe partial JSON.

Exports are deliberately separate from the hidden RAW library. PNG and JPEG
exports are published to `Pictures/AuRaw`. Android 10 and newer use
`MediaStore.Images` with that relative path, so gallery apps can discover the
result without a permission prompt. Android 8 and 9 write to the same public
Pictures folder, request legacy write permission only when needed for export,
and notify the media scanner. No export destination dialog is shown.

eframe is forced to use wgpu. wgpu includes Vulkan and GLES backends on
Android, and AuRaw requests the adapter's actual 2D texture-size limit so RAWs
wider than eframe's usual 8192-pixel default can be processed when the device
supports them. Pass 3 reuses pass 1's intermediate texture, saving eight bytes
of GPU allocation per RAW pixel (about 192 MB for a 24-megapixel file). Very
large RAW files still require substantial device GPU memory.

## In-app diagnostics

The Settings tab includes a **Diagnostics** field with a **Copy log** button.
It records the Android model and ABI, selected wgpu backend and GPU driver,
RAW white balance / black and white levels / camera matrix, sampled input
fingerprints, preview decode and proxy timings, and tiled-export preparation
timings. Open the same RAW and run an export before copying the report from
each device.

## Linux remains unchanged

On the computer where LibRaw is installed in `/usr/local`, continue to use:

```sh
LIBRARY_PATH=/usr/local/lib \
PKG_CONFIG_PATH=/usr/local/lib/pkgconfig \
cargo run -p auraw-ui --bin auraw --release
```

On a computer without desktop LibRaw, the build now fails by default. For a
deliberate non-production check that does not exercise RAW loading, explicitly set
`AURAW_ALLOW_NO_LIBRAW=1` (or the exact value `true`). Values such as `0`,
`false`, or an empty value do not disable the requirement. Production and release
builds must provide LibRaw.

Normal local commands such as `cargo run -p auraw-ui --bin auraw --release` may build uncommitted work.
The reproducible Linux and Android release scripts still require a clean Git
checkout, embed the full commit ID, and discard native output if the source
changes while compilation is running.

## Task-progress notifications

On Android 13 and newer, AuRaw asks for notification permission when the first
long-running operation starts. When allowed, one ongoing notification mirrors
the active task's phase and progress. Android 8 through 12 do not require this
runtime permission.

This notification does not yet run the task in an Android foreground service.
Keep AuRaw open in the foreground until the operation finishes; leaving or
closing the app may suspend or stop the task.
