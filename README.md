# AuRaw

GPU-based RAW image editor for Linux, Windows, and Android.

AuRaw is an independent Rust/egui implementation whose product direction was
inspired by the open-source RAW-editing work of
[darktable](https://www.darktable.org/), [Ansel](https://ansel.photos/), and
[RapidRAW](https://github.com/CyberTimon/RapidRAW). Source-derived darktable and
Ansel algorithms are identified in file headers and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). RapidRAW is acknowledged as
interface and workflow inspiration; its AGPL-3.0 source code is not relicensed
as part of AuRaw.


## Platform support

- **Linux (supported):** primary desktop development and AppImage packaging target.
- **Windows (supported):** x86_64 GNU build with LibRaw, produced by the Windows workflow.
- **Android (experimental):** arm64 application build; device GPU and camera compatibility vary.

## RAW library

AuRaw opens on the Library tab. On desktop, choose **Open Folder…** to scan a
folder recursively; embedded LibRaw previews are decoded in the background as
their rows become visible. Selecting a thumbnail opens that RAW in Develop.
The catalog, visible rows, pending preview work, and GPU thumbnail cache are
bounded so large photo folders do not need to be loaded into memory at once.

On Android, the floating **+** button imports through the system document
picker. Android 10 and newer keep imported RAW files in the user-visible
`Download/AuRaw` collection without requesting broad storage permission. See
[ANDROID.md](ANDROID.md) for the Android 8–9 fallback and scoped-storage details.

Develop edits are non-destructive. AuRaw restores exposure, color, effects,
local masks, and lens selection from a versioned `<raw filename>.auraw`
sidecar, then saves committed edits in the background. **Save Edits** (or
Ctrl/Cmd+S) forces an immediate retry without modifying the RAW. Automatic
saves use a short 0.9-second, non-sliding coalescing interval and wait for the
current interaction to finish; snapshotting is O(1), while serialization and
storage I/O stay on a worker. Desktop
sidecars sit beside the source file; Android sidecars are visible siblings in
the AuRaw library folder.

## DCP camera profiles

Desktop builds can point **Settings > RAW color profiles > Camera profile folder**
at a top-level profile library such as Adobe Camera Raw's `CameraProfiles`
directory. AuRaw scans that root recursively, so one folder can contain profiles
for many camera models and any number of DCP variants per camera. Matching uses
the DCP camera model metadata first and falls back to camera/model names in the
profile path when required.

When the current camera has more than one matching DCP, **Develop > Adjustments**
shows a **Camera profile** dropdown. The default Automatic choice uses AuRaw's
preferred neutral/standard match; selecting another profile reloads the RAW with
that DCP while preserving the live edits. Explicit per-image profile choices are
stored in the `.auraw` sidecar as a path relative to the configured profile root,
so reopening the image restores the same rendering choice.

## Linux (Debian/Ubuntu)

Install the build dependencies:

```sh
sudo apt update
sudo apt install build-essential pkg-config libclang-dev libraw-dev \
  liblensfun-dev liblensfun-data-v1 \
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
- Android SDK 35, Build Tools 35.0.0, NDK 28.2.13676358, and CMake 3.22.1
- `libclang` and `cargo-ndk` 4.1.2

Set up Rust and the Android environment:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked

export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/28.2.13676358"
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

## Lens corrections and control reset

The **Adjustments > Optics** section uses Lensfun profiles to correct lens
distortion, lateral chromatic aberration, and vignetting before the normal RAW
processing stack. AuRaw reads the camera, lens, focal length, and aperture from
the RAW metadata. When Lensfun finds one unambiguous lens profile, correction
is enabled automatically while the RAW is opening. The Brand and Lens menus can
be used to choose or override the profile, and the Enabled checkbox switches
between corrected and original RAW geometry. Changing geometry clears local
masks because their coordinates no longer refer to the same pixels.

Desktop builds discover Lensfun through `pkg-config`. Packaged builds may ship a
database beside the executable in `lensfun/` or under
`share/auraw/lensfun/`; `AURAW_LENSFUN_DB` takes priority during database
discovery. Builds without Lensfun keep the Optics section visible but disabled.
Android currently builds without native Lensfun support.

Double-click any adjustment slider, its label/value field, or a color wheel to
restore the value captured when that control was first shown.

## Perceptual Color Mixer

The Red, Orange, Yellow, Green, Aqua, Blue, Purple, and Magenta controls keep
Lightroom's Hue/Saturation/Luminance interface, but they do not process pixels
in mathematical HSL. The GPU pipeline uses a full-precision staged scene-linear color mixer:

- hue selection in perceptual OKLab with neutral and deep-shadow protection;
- an edge-aware 3x3 preview / 5x5 desktop selector that suppresses demosaic
  chroma speckle without blurring center-pixel detail;
- constant-lightness, constant-hue gamut compression for hue and saturation;
- RGB-ratio-preserving scene-linear gain for color luminance;
- an exact bypass when every Color Mixer slider is zero.

This avoids the pixel-brightness noise caused by adding directly to HSL
lightness while retaining the existing Rec.2020 scene pipeline and darktable
sigmoid display transform.

## Perceptual Color Grading

The Develop panel and every local mask include four interactive color wheels:
Shadows, Midtones, Highlights, and Global. Each wheel combines hue,
saturation, and a separate luminance control, with Lightroom-style Blending
and Balance controls for the three tonal ranges.

Color grading is evaluated at full floating-point precision in the
scene-linear Rec.2020 pipeline before the darktable sigmoid display transform.
The grading tint is composed in perceptual OKLab, with smooth log-luminance
range masks, deep-shadow signal protection, an HDR shoulder, and constant-hue
positive-gamut compression instead of per-channel clipping. Wheel luminance
uses a scene-linear RGB-ratio-preserving exposure gain. Neutral grading is an
exact bypass, and masked grading uses the same normalized full-image mask
atlas for preview proxies and tiled exports.


## Lightroom-style Effects and interface

Texture, Clarity, and Dehaze run in a dedicated local-effects pass after the
Light and point-curve controls. The pass reads a full-precision developed base
texture, so local residuals are never calculated against an earlier/raw stage.
Texture uses a noise-aware fine-detail band, Clarity uses an edge-aware
mid-scale à-trous band, and Dehaze uses a scale-aware dark channel with a
stable full-image ambient-light estimate. Flat fields are an exact no-op for
Texture and Clarity, and zoom crops inherit the full-frame tonal anchors so
adaptive controls do not change merely because the image is panned.

The standard interface shows Light, Tone Curve, Color, Effects, and Color
Mixer. Settings contains an **Expert mode** checkbox, disabled by default.
Enabling it reveals the darktable sigmoid internals, RAW/demosaic controls, and
highlight-reconstruction settings. The default point curve contains only its
two endpoints; users add interior points explicitly.

## Creative effects

The Effects panel also contains a highlight-aware **Glow** and a post-crop
**Vignette**. Glow extracts bright sources from the completed developed image
and spreads them through a continuous five-stage diffusion cascade without
lifting flat shadows or producing sparse sampling rings. Standard mode exposes
Glow Amount; Expert mode adds Radius and Threshold.

Vignette exposes Amount, Midpoint, Roundness, Feather, and Highlights. It uses
full-image coordinates for stable geometry across previews and tiled exports,
and applies a hue-preserving scene-linear gain.

## Sidebar and export

The Develop sidebar is divided into four tabs:

- **Adjustments** contains the complete photographic processing stack.
- **Masks** provides Lightroom-style Brush, Radial Gradient, Linear Gradient, Subject, Background, Object, Luminance Range, and Color Range masks with add, subtract, and intersect submasks. Object selection uses a hard-edged Size brush, a brush-footprint focus box with automatic background guards, adaptive crop expansion, cached SAM 2.1 image embeddings, clean replacement strokes after refinement, connected-component cleanup, touch-safe pinch cancellation, and automatic ViTMatte alpha-matting refinement for hair/fur/semi-transparent boundaries. Subject and Not Subject masks use the same ViTMatte boundary refiner automatically after BiRefNet. Landscape and Depth Range remain future tools. Background currently means the inverse of the subject probability and is labeled accordingly in the mask UI.
- **Inpainting** provides local LaMa-based object removal. It processes a bounded context crop locally, then stores only the affected full-resolution scene-linear patch in the edit sidecar.
- **Export** contains the PNG export action and all output options.

PNG sizing can use the original dimensions, long edge, short edge, width,
height, or a percentage while preserving the source aspect ratio. Upscaling is
disabled by default. Final-size resampling happens only after demosaic and tone
processing, in display-linear Rec.2020, before a single sRGB output encoding;
the sensor mosaic is never resized for final output. Export dimensions, pixel
counts, tile working sets, and temporary files are bounded, and the destination
is published only after a complete PNG has been written. The **Keep metadata**
option embeds the available source filename, camera make/model, original
dimensions, output dimensions, software, and normalized orientation in PNG iTXt
and eXIf metadata.

## AI-mask runtime trust

Desktop Subject and Object selection never download or extract native runtime code.
Choose a local ONNX Runtime library in Settings; AuRaw records its SHA-256 and
revalidates that exact file before every dynamic load. Each segmentation model is separately pinned by SHA-256, downloaded through a
temporary file, revalidated on cache hits, and atomically moved into the cache
only after verification. Subject selection uses BiRefNet followed by a pinned ViTMatte ONNX edge refiner; Object selection uses
the separate SAM 2.1 Hiera Tiny encoder and prompt decoder ONNX files, followed automatically by the same ViTMatte refiner. On Linux,
AI selection stays disabled
until a local runtime has been selected and pinned.

## Resource limits

RAW dimensions, sensor dimensions, file size, embedded ICC data, model input,
model output, export dimensions, export pixels, and export row buffers are
checked before allocation or unpacking. Oversized or inconsistent inputs fail
with an error rather than allowing unbounded memory or storage growth.


## Development quality gates

Pull requests run Rust formatting, Clippy with warnings denied, all Rust tests (including WGSL parse/validation), the complete Python suite, source-connectivity checks, and deterministic renders of the committed CC0 Bayer and X-Trans fixtures. See `regression/README.md` and `benchmarks/README.md`. Dependency policy is documented in [DEPENDENCIES.md](DEPENDENCIES.md), with bundled-license details in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## License and attribution

AuRaw is copyright 2026 Duecki and AuRaw contributors and is distributed under
the [GNU General Public License, version 3 or later](COPYING)
(`GPL-3.0-or-later`). There is no warranty, to the extent permitted by law.

Some algorithms are adapted from GPL-compatible upstream projects and retain
their original copyrights. Model weights, native libraries, profile databases,
and other third-party components keep their own licenses. See
[NOTICE.md](NOTICE.md), [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), and
[DEPENDENCIES.md](DEPENDENCIES.md) before redistributing a packaged build. The
bilingual [privacy notice](PRIVACY.md) explains local image processing and the
optional third-party model downloads.
