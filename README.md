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


## Lightroom-style Effects and interface

Texture, Clarity, and Dehaze run in a dedicated local-effects pass after the
Light and point-curve controls. The pass reads a full-precision developed base
texture, so local residuals are never calculated against an earlier/raw stage.
Texture uses a noise-aware fine-detail band, Clarity uses an edge-aware
mid-scale à-trous band, and Dehaze uses a local dark-channel/airlight model.
Flat fields are an exact no-op for Texture and Clarity.

The standard interface shows Light, Tone Curve, Color, Effects, and Color
Mixer. Settings contains an **Expert mode** checkbox, disabled by default.
Enabling it reveals the darktable sigmoid internals, RAW/demosaic controls, and
highlight-reconstruction settings. The default point curve contains only its
two endpoints; users add interior points explicitly.

## Creative effects

The Effects panel also contains a highlight-aware **Glow** and a post-crop
**Vignette**. Glow blooms bright sources from the completed developed image
without lifting flat shadows. Standard mode exposes Glow Amount; Expert mode
adds Radius and Threshold.

Vignette exposes Amount, Midpoint, Roundness, Feather, and Highlights. It uses
full-image coordinates for stable geometry across previews and tiled exports,
and applies a hue-preserving scene-linear gain. See
[CREATIVE_EFFECTS.md](CREATIVE_EFFECTS.md) for the processing details.

## Sidebar and export

The Develop sidebar is divided into four tabs:

- **Adjustments** contains the complete photographic processing stack.
- **Masks** provides Lightroom-style Brush, Radial Gradient, and Linear Gradient masks with add, subtract, and intersect submasks. Brush includes paint/erase modes, size, and feather; local adjustments are applied in the scene-linear GPU pipeline. Subject, Background, Object, Landscape, Luminance Range, Color Range, and Depth Range appear as future-tool placeholders.
- **Inpainting** is reserved for future healing and object removal.
- **Export** contains the PNG export action and all output options.

PNG sizing can use the original dimensions, long edge, short edge, width,
height, or a percentage while preserving the source aspect ratio. Upscaling is
disabled by default. The **Keep metadata** option embeds the available source
filename, camera make/model, original dimensions, output dimensions, software,
and normalized orientation in PNG iTXt and eXIf metadata.
