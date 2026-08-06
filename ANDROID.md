# Android build

AuRaw's Android app uses the same Rust UI, LibRaw loader, WGSL shaders, and
wgpu processing pipeline as the Linux app. Android-specific code is limited to
the application entry point and the system file-picker bridge.

## Prerequisites

- Rust with the Android target for the device (normally
  `aarch64-linux-android`)
- Android SDK 35 and Android NDK 28.2.13676358
- Android SDK CMake 3.22.1 (with its bundled Ninja), a JDK, host `libclang`, and Gradle 8.11.1 or newer in the Gradle 8 series (CI uses 8.11.1). The first Lensfun build automatically bootstraps a pinned Meson build tool in `android/native/tools/`.
- `cargo-ndk` 4.1.2 (`cargo install cargo-ndk --version 4.1.2 --locked`)

Set `ANDROID_SDK_ROOT` and `ANDROID_NDK_HOME`, for example:

```sh
export ANDROID_SDK_ROOT="/home/duecki/Android/Sdk"
export ANDROID_NDK_HOME="/home/duecki/Android/Sdk/ndk/28.2.13676358"
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

For a command-line build from the repository root:

```sh
gradle assembleDebug -PaurawAbi=arm64-v8a
```

Running the same command from the `android` directory remains supported.

The debug APK is written to `android/app/build/outputs/apk/debug/app-debug.apk`.

### 16 KB memory-page support

Android builds use NDK r28c, whose linker emits 16 KB-compatible ELF LOAD
segments by default. AGP 8.9.2 packages JNI libraries uncompressed with legacy
packaging disabled, allowing the APK to retain 16 KB zip alignment for direct
loading on Android 15+ devices that use 16 KB memory pages. The manifest does
not force native-library extraction.

After building an APK, verify both ELF LOAD alignment and APK zip alignment with:

```sh
python3 scripts/dev.py verify-android-16kb android/app/build/outputs/apk/debug/app-debug.apk
```

The verifier checks every 64-bit `.so` with the pinned NDK's `llvm-objdump` and
runs Build Tools 35.0.0 `zipalign -c -P 16 -v 4`. CI runs the same check on the
arm64 debug APK. Runtime testing should also be performed on a 16 KB Android 15
or newer device/emulator (`adb shell getconf PAGE_SIZE` must report `16384`).

To build only the native LibRaw, Lensfun, and Rust libraries without packaging an APK:

```sh
python3 scripts/dev.py build-android arm64-v8a release
```

The first native build downloads LibRaw 0.22.1 and Lensfun 0.3.4, then
cross-compiles static libraries with the pinned NDK. Lensfun's profile XML
database is staged as APK assets and copied to app-private storage on first
launch, because Lensfun loads its database from filesystem paths. Generated
sources, libraries, and APKs are ignored by Git. The native build
also copies `libc++_shared.so` from the pinned NDK instead of storing it in the
repository. No LibRaw installation on the Linux host is required. LibRaw is
cached for development builds until its version, ABI, API level, or NDK
revision changes; set `AURAW_REBUILD_LIBRAW=1` to force a clean native rebuild.
Set `AURAW_REBUILD_LENSFUN=1` to rebuild the cached Lensfun and GLib libraries.
Release builds always discard the ignored native cache and rebuild it from the
pinned source revisions.

Other supported ABI names are `armeabi-v7a`, `x86`, and `x86_64`. Build and
package one ABI at a time by passing the same name to the script and the Gradle
`aurawAbi` property.

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
cargo run --release
```

On a computer without desktop LibRaw, the build now fails by default. For a
deliberate non-production check that does not exercise RAW loading, explicitly set
`AURAW_ALLOW_NO_LIBRAW=1` (or the exact value `true`). Values such as `0`,
`false`, or an empty value do not disable the requirement. Production and release
builds must provide LibRaw.

Normal local commands such as `cargo run --release` may build uncommitted work.
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
