# AuRaw

GPU-based RAW image editor for Linux, Windows, macOS, and Android.

AuRaw is an independent Rust/egui implementation whose product direction was
inspired by the open-source RAW-editing work of
[darktable](https://www.darktable.org/), [Ansel](https://ansel.photos/), and
[RapidRAW](https://github.com/CyberTimon/RapidRAW). Source-derived darktable and
Ansel algorithms are identified in file headers and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). RapidRAW is acknowledged as
interface and workflow inspiration; its AGPL-3.0 source code is not relicensed
as part of AuRaw.


## Platform support

- **Linux (recommended):** primary desktop development target; the Gitea Actions workflow builds the x86_64 AppImage.
- **Android (recommended):** arm64 application build; device GPU and camera compatibility vary.

- **Windows (supported):** x86_64 GNU build with LibRaw, produced by the Windows workflow.
- **macOS (available):** native Apple Silicon and Intel application bundles with
  LibRaw and Lensfun, produced by the macOS GitHub workflow.

## RAW library

AuRaw opens on the Library tab. On desktop, choose **Open Folder…** to scan a
folder recursively; embedded LibRaw previews are decoded in the background as
their rows become visible. Selecting a thumbnail opens that RAW in Develop.
The catalog, visible rows, pending preview work, and GPU thumbnail cache are
bounded so large photo folders do not need to be loaded into memory at once.
On desktop, generated library thumbnails live in AuRaw's private per-user
operating-system cache (for example Local AppData, Library/Caches, or the XDG
cache directory), never in the selected photo folder.

On Android, the floating **+** button imports through the system document
picker. Imported RAW files are copied into AuRaw's hidden app-media library at
`Android/media/de.duecki.auraw/.library`, without requesting broad storage
permission. Exported images are separate and are published to `Pictures/AuRaw`
so gallery apps can discover them. See [ANDROID.md](ANDROID.md) for migration
and scoped-storage details.

Rendered `.tif`/`.tiff` files are also supported. AuRaw distinguishes rendered
RGB TIFFs from CFA/DNG-style sensor TIFFs before decoding, honors embedded ICC
profiles, preserves float HDR headroom, and exports 8/16-bit ICC-managed TIFF or
32-bit linear Rec.2020 masters. See [`docs/TIFF.md`](docs/TIFF.md) for the import,
color-management, resource-limit, and export-strip contract.

Develop edits are non-destructive. AuRaw restores exposure, color, effects,
local masks, and lens selection from a versioned `<raw filename>.auraw`
sidecar, then saves committed edits in the background. **Save Edits** (or
Ctrl/Cmd+S) forces an immediate retry without modifying the RAW. Automatic
saves use a short 0.9-second, non-sliding coalescing interval and wait for the
current interaction to finish; snapshotting is O(1), while serialization and
storage I/O stay on a worker. Desktop
sidecars sit beside the source file; Android sidecars are hidden siblings of
the imported RAW inside AuRaw's `.library` folder.

## AuRaw Cloud

AuRaw can also browse a self-hosted AuRaw Cloud server. Enable it in Settings,
enter the server address (for example `192.168.1.20:8787`) and optional access
token, then test the connection. Desktop adds **Cloud** to the Library folder
sidebar; Android adds **Local** and **Cloud** tabs above the Library.

Cloud browsing transfers only catalog metadata and 512px JPEG previews. The
full RAW and current `.auraw` sidecar are downloaded into AuRaw's private cache
only when the image is opened. Autosave keeps that cached sidecar safe locally
and uploads it with a version precondition; if another client saved first,
AuRaw reports a conflict instead of silently replacing those edits. A rendered
developed thumbnail is uploaded against the same sidecar revision.

Every cloud-card click revalidates that asset's current RAW, sidecar, and
thumbnail versions before opening, so edits made by another client do not
require a manual Library refresh. The latest successful catalog and previews
are kept for offline browsing. A RAW can be opened offline after it has been
downloaded once on that device; offline edits are saved in the private cache,
shown as **waiting to sync**, and retried when the image is next opened or
saved with the server reachable. Cloud thumbnails show a cloud icon while the
RAW still needs downloading and a download icon once it is available offline.

In the Cloud library, use the floating **+** button to select and upload one or
more RAW files. Desktop also sends a matching `.auraw` sidecar and current
developed thumbnail when present. Android streams each selected document
straight to the server through the system picker and does not add another copy
to the Local library. Upload progress and the final success/failure summary are
shown in the Library.

Cloud libraries can use nested folders. Desktop shows the hierarchy in the
folder sidebar, while the Library breadcrumb and child-folder buttons provide
the same navigation on Android. **New folder** and **Paste here** act on the
folder currently being viewed. Folder menus support copy, cut, paste, rename,
move, and recursive deletion; desktop folders can also be dragged onto another
cloud folder.

Cloud RAW cards support the normal selection workflow. Use **Select** on
desktop or long-press a card on Android, then add as many RAWs as needed.
Desktop right-click menus and Android's selection overflow menu support export,
copy/cut/paste, duplicate, rename, reset, and delete. Export downloads only the
selected RAWs and sidecars into the existing private cache before running the
same batch exporter used by the Local library.

The image clipboard is shared between Local and Cloud. Copy or cut one or more
RAWs, switch tabs or folders, and use **Paste** in a local or cloud destination.
Local-to-local copies, uploads, and downloads preserve the matching `.auraw`
sidecar and choose a collision-safe filename instead of overwriting an existing
RAW. A cut removes its source only after the destination has been completed.
If only part of a multi-file cut succeeds, the clipboard keeps only the RAWs
that still need moving, so **Paste** can be retried without duplicating work.

The Docker server project lives beside this checkout in `AuRaw-2.0-Server`.
Its README covers startup, token authentication, imports, HTTPS deployment,
storage, and backups.

The Basic **Contrast** control keeps its familiar -100% to +100% range, with 0%
neutral, and maps to darktable's normal sigmoid slider range: -100% is 0.7, 0%
is the 1.5 default, and +100% is 3.0. Darktable's wider
0.1–10.0 expert limits are deliberately not used as Basic-slider endpoints.
Sidecars from earlier AuRaw processes migrate to this percentage-backed
parameter when opened.

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

## Cargo workspace

The repository is organized as six crates: `auraw-core`, `auraw-gpu`, `auraw-ai`,
`auraw-ui`, `auraw-ffi`, and `auraw-cli`. See
[`docs/CARGO_WORKSPACE.md`](docs/CARGO_WORKSPACE.md) for ownership rules, the
dependency graph, and workspace-wide validation commands.

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
cargo run -p auraw-ui --bin auraw --release
```

## Android

Install:

- JDK 17
- Gradle 8.11.1 or newer in the Gradle 8 series
- Android SDK platform, Build Tools, and NDK versions declared in `Cargo.toml` `[workspace.metadata]`, plus CMake 3.22.1
- `libclang` and `cargo-ndk` 4.1.2

Set up Rust and the Android environment:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked

export ANDROID_SDK_ROOT="$HOME/Android/Sdk"
eval "$(cargo xtask print-metadata --format shell)"
export ANDROID_NDK_HOME="$ANDROID_SDK_ROOT/ndk/$AURAW_ANDROID_NDK_VERSION"
```

Build the APK from the repository root:

```sh
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
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
Android builds Lensfun and bundles its profile database with the APK.

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
highlight-reconstruction settings. Inpaint opposed is the default for new RAWs
and for supported edits loaded from older sidecars. The default point curve contains only its
two endpoints; users add interior points explicitly.

RGB channel curves permit ascending or descending output segments. Their
scene-domain extension below zero uses the signed slope of the first segment, so
signed channel intermediates cross zero without a derivative mismatch. Extreme
black endpoints near the top of the editor enter a finite C1 scene shoulder;
the first segment's scene-domain slope is limited consistently on both sides of
zero to prevent half-float precision from producing extreme false-colour steps.
Raising the composite curve's black endpoint establishes a neutral lifted-black
floor;
this intentionally reduces shadow colorfulness as colored signals approach
absolute black rather than preserving RGB ratios all the way to that floor. If
the composite curve's first segment descends below a lifted endpoint, the floor
makes its effective endpoint slope zero on both sides of zero luminance.

## Creative effects

The Effects panel also contains a highlight-aware **Glow** and a post-crop
**Vignette**. Glow extracts bright sources from the completed developed image
and spreads them through a continuous five-stage diffusion cascade without
lifting flat shadows or producing sparse sampling rings. Standard mode exposes
Glow Amount; Expert mode adds Radius and Threshold.

Vignette exposes Amount, Midpoint, Roundness, Feather, and Highlights. At the
default 50 / 0 / 50 / 0 values, their combined shape follows curves measured
from Lightroom's default vignette. Dark edges multiply toward black while
positive edges blend toward white in display-linear RGB. Darktable-style
independently normalized frame axes keep the default falloff consistent across
aspect ratios, previews, crops, and tiled exports.

## Sidebar and export

The Develop sidebar is divided into four tabs:

- **Adjustments** contains the complete photographic processing stack.
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

## Local AI runtime trust

Choose a local ONNX Runtime library in Settings; AuRaw records its SHA-256 and
revalidates that exact file before every dynamic load. Each segmentation model is separately pinned by SHA-256, downloaded through a
temporary file, revalidated on cache hits, and atomically moved into the cache
only after verification. Subject selection uses the explicitly selected BiRefNet General-Lite, Lite-2K, or HR checkpoint and preserves its soft output directly; Object selection uses
AI selection stays disabled
until a local runtime has been selected and pinned.

Detail's optional **AI Denoise** uses the GPL-3.0 RawNIND UtNet2 package from
the published darktable-ai 5.6 release. The first enable asks before contacting
GitHub. AuRaw pins the complete `.dtmodel` archive and both extracted ONNX
graphs by SHA-256. Bayer RAWs use the joint denoise/demosaic graph, then remosaic
its result and run AuRaw's normal demosaic stage as darktable does; X-Trans uses
the declared linear Rec.2020 graph. Inference is overlap-tiled locally, and only
the checkbox is persisted—the half-float derived camera-RGB cache is rebuilt
from the original mosaic and enters the ordinary non-destructive pipeline
before capture sharpening. Standard denoise values remain saved and are
restored unchanged when AI Denoise is disabled.

## Resource limits

RAW dimensions, sensor dimensions, file size, embedded ICC data, model input,
model output, export dimensions, export pixels, and export row buffers are
checked before allocation or unpacking. Oversized or inconsistent inputs fail
with an error rather than allowing unbounded memory or storage growth.


## Development quality gates

Pull requests run Rust formatting, Clippy with warnings denied, all Rust tests (including WGSL parse/validation), the complete Python suite, source-connectivity checks, and deterministic renders of the committed CC0 Bayer and X-Trans fixtures. See `docs/DEVELOPMENT.md` and `regression/README.md`. Dependency policy is documented in [DEPENDENCIES.md](DEPENDENCIES.md), with bundled-license details in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

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
