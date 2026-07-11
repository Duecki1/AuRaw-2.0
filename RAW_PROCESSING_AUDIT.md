# AuRaw 2.0 raw-processing audit

Audit date: 2026-07-11

## Executive conclusion

The reviewed pipeline is a promising GPU-first preview renderer, but it is **not yet equivalent in image quality to darktable/Ansel or Adobe Camera Raw/Lightroom**. The main gap is not slider tuning: it is missing or approximate sensor calibration, demosaic, camera-profile, denoise, lens, sharpening, color-management, and export infrastructure.

This patch fixes correctness and safety defects that could produce banding, color casts, malformed metadata reads, invalid calibration ranges, or silently wrong output. It does **not** claim Lightroom output matching. Adobe's raw profiles and several processing algorithms are proprietary, so the defensible target is competitive quality verified on a shared RAW corpus, not byte-for-byte parity.

## Implemented fixes

### 1. Full LibRaw black-level calibration

**Severity: critical**

The original loader used only `black + cblack[0..4]`. LibRaw also exposes an optional repeating black-level pattern in `cblack[4]`, `cblack[5]`, and `cblack[6..]`. Ignoring it leaves row/column or tiled fixed-pattern residuals before white balance and demosaic.

Implemented:

- Builds an effective black value for every active-area photosite.
- Uses active-area coordinates before orientation, as required by the DNG black-pattern origin.
- Uploads the map as an `R32Float` GPU texture.
- Applies it in both normal raw sampling and pre-demosaic highlight reconstruction.
- Validates that every photosite has `white > black`.

Affected files:

- `src/pipeline/raw_loader.rs`
- `src/pipeline/gpu.rs`
- `src/shaders/common.wgsl`
- `src/shaders/raw_sampling.wgsl`
- `src/shaders/highlights.wgsl`

### 2. Safe decoded-row access

**Severity: critical for malformed input**

The original unsafe pointer arithmetic trusted `raw_pitch`, unchecked row-offset multiplication, and clamped invalid LibRaw color indices into a valid range.

Implemented:

- Rejects pitch smaller than a decoded row.
- Rejects non-`u16`-aligned pitch.
- Uses checked crop, output-size, and row-offset arithmetic.
- Rejects invalid CFA indices rather than converting them into plausible but wrong colors.
- Rejects references to an undescribed CFA plane.

### 3. Correct handling of `linear_max`

**Severity: high**

The original code preferred the first non-zero `linear_max` as a shared fallback and trusted each non-zero plane unconditionally. LibRaw documents `maximum` as the decoded saturation value and `linear_max` as optional metadata whose black value is not subtracted. Real files may contain unusable values.

Implemented:

- Uses decoded `maximum` as the primary fallback.
- Uses a per-plane `linear_max` only when it is above that plane's black level and does not exceed a reported shared maximum.
- Adds regression tests for invalid values.

### 4. Reject unsupported mosaics instead of silently miscoloring them

**Severity: high**

LibRaw color descriptors can be RGBG, RGBE, GMCY, GBTG, and other layouts. AuRaw's shaders are RGB-only. The old fallback mapped unknown planes by numeric index, which could turn non-RGB mosaics into convincing but incorrect RGB images.

Implemented:

- Accepts only descriptors with one red, one blue, and one or two green planes.
- Keeps G1 and G2 calibration separate.
- Rejects non-RGB mosaics with an explicit error.
- Ignores unused profile rows instead of folding them into blue.
- Continues to reject full-color/linear, Leaf CatchLight, and unsupported special CFA codes.

### 5. Geometry and orientation validation

**Severity: high for affected cameras**

AuRaw does not yet implement the geometry-resampling stage required for non-square photosites or special sensor layouts. Rendering them as ordinary square Bayer data creates distorted output.

Implemented:

- Rejects invalid or non-unity LibRaw pixel aspect ratios.
- Validates documented orientation codes `0`, `3`, `5`, and `6`.
- Uses checked crop-end arithmetic.
- Adds coordinate tests for both 90-degree rotations.

This is deliberately fail-fast. Supporting these files correctly remains a larger implementation item.

### 6. More stable camera-matrix inversion

**Severity: high**

The old 3x3 normal-equation inversion used `f32` Gauss-Jordan elimination without pivoting. A small or zero diagonal pivot could cause failure even when a later row contained a valid pivot.

Implemented:

- Performs inversion in `f64`.
- Uses partial pivoting.
- Rejects non-finite or singular results.
- Refuses to silently treat camera RGB as the Rec.2020 working space when no usable matrix exists.

This improves numerical robustness; it does not replace a complete DNG/DCP profile engine.

### 7. Bounded camera-string parsing

**Severity: medium / memory safety**

The original `CStr::from_ptr` call assumed fixed-size LibRaw make/model arrays were NUL-terminated. Malformed metadata that fills the complete array could read beyond the array.

Implemented bounded conversion within the provided slice.

### 8. Calibration and GPU input validation

**Severity: medium**

Added validation for:

- Non-zero, non-overflowing dimensions.
- Exact raw, CFA, and black-map lengths.
- CFA indices in `0..=3`.
- Finite positive white-balance coefficients.
- Finite and non-empty camera matrices.
- Finite black/white metadata and a positive range at every photosite.

### 9. Black-point UI/shader consistency

**Severity: medium**

The UI exposed `-1.0..1.0`, while both shaders clamp the value to `-0.25..0.25`; most of the slider was therefore a dead range.

Both UI implementations now expose `-0.25..0.25`, matching the actual sensor-domain correction.

## Comparison with darktable and Ansel

### Decode and sensor calibration

After the patch, AuRaw handles LibRaw's shared, per-plane, and repeating black levels correctly for the supported single-channel RGB CFA path. It still lacks several pre-demosaic corrections present in mature raw processors: masked-pixel/optical-black modeling, bad-pixel maps, PDAF correction, green equilibration, row/column-noise correction, lens shading, and camera-specific sensor quirks.

### Bayer demosaic

AuRaw's four WGSL passes are **RCD-inspired**, not a verified port of darktable's RCD implementation. The current shader uses custom directional statistics, interpolation, chroma cleanup, and clamped edge sampling. darktable's implementation includes its own staging, border policy, and fallback behavior. Therefore the current path cannot be called RCD-equivalent without a reference-port review and image tests.

Expected visible differences include:

- zippering and false color on high-frequency edges;
- detail loss or maze artifacts;
- different behavior near the image border;
- noise-dependent chroma artifacts;
- different highlight-edge behavior.

### X-Trans demosaic

This is the largest immediate image-quality gap. AuRaw's X-Trans path is a custom seed/green/chroma/refine sequence. darktable exposes Markesteijn one-pass and three-pass variants, VNG, frequency-domain chroma, and dual-demosaic options. The current AuRaw implementation should be considered a preview-quality custom demosaic, not Markesteijn-equivalent.

### Highlight reconstruction

The Bayer opponent-color method is partly translated from Ansel's highlight code, while the multiscale guided solver is application-specific. Translation of one method does not establish whole-module equivalence: clipping masks, borders, CFA support, color adaptation, iteration behavior, and interaction with demosaic all need reference tests.

### Camera color and profiles

AuRaw currently builds a camera-to-Rec.2020 matrix from LibRaw matrices and performs limited dual-illuminant interpolation. This is materially less complete than a full DNG/DCP or ICC camera-profile pipeline.

Missing or incomplete items include:

- `ForwardMatrix1/2` handling;
- `AnalogBalance` in the DNG color model;
- `ReductionMatrix1/2` for more than three camera planes;
- robust illuminant interpolation from chromaticity rather than nearest WB-table CCT;
- full EXIF light-source coverage;
- DCP Hue/Saturation maps, LookTable, profile tone curve, baseline exposure, and profile embeds;
- camera-specific profile selection and user profiles;
- profile validation and test-chart regression.

Adobe explicitly makes raw profiles a foundation for color and tonality, including Adobe Raw, Camera Matching, and Adaptive profiles. A single matrix plus custom HSL cannot reproduce those renderings.

### Tone, presence, and color mixer

The current tone map, local guide, texture, clarity, dehaze, saturation/vibrance, and HSL mixer are custom approximations. The UI resemblance to Lightroom does not imply equivalent semantics.

Notable risks:

- HSL is computed on clipped/scaled linear sRGB rather than a perceptually uniform, gamut-aware color model;
- local texture/clarity uses small brute-force luminance kernels and can differ in halos/noise response;
- dehaze is a local dark-channel approximation without mature global estimation and guided refinement;
- gamut compression is simple luminance-to-peak desaturation rather than profile-aware gamut mapping;
- tone behavior has not been fitted against calibrated targets or reference processors.

### Denoise, sharpening, lens correction, and output

These are not at mature-raw-processor level:

- chroma denoise is a simple post-demosaic operation;
- there is no calibrated raw noise model or camera/ISO noise profiles;
- no capture sharpening and no output sharpening pipeline;
- chromatic-aberration controls are manual rather than profile-driven;
- no distortion/vignetting/TCA lens-profile pipeline;
- preview output is 8-bit sRGB, with no complete high-bit-depth export and ICC/soft-proof workflow.

Adobe documents sharpening, noise reduction, lens-defect correction, profiles, and workflow/output options as normal parts of Camera Raw processing. Those are necessary for a fair quality comparison.

## Required larger implementations, in priority order

### P0 — necessary before claiming competitive raw quality

1. **Reference demosaic ports**
   - Port darktable RCD faithfully, including border handling and fallback.
   - Port Markesteijn three-pass for X-Trans; add a fast preview mode and optional dual/frequency-domain cleanup.
   - Keep algorithm provenance and licensing clear.

2. **Image-quality regression suite**
   - Curate real RAW files across Bayer/X-Trans, ISO, exposure, cameras, saturated lights, foliage, fabrics, stars, skin, and resolution charts.
   - Generate fixed reference renders from pinned darktable/Ansel versions and controlled Adobe exports where licensing permits.
   - Compare linear intermediate TIFFs plus final renders, using visual crops and metrics such as ΔE on charts, edge MTF, false-color energy, PSNR/SSIM where meaningful, and highlight hue error.
   - Add golden tests for black level, white level, CFA orientation, matrices, and clipping masks.

3. **Complete camera-profile engine**
   - Implement the full DNG color model and DCP profile stages.
   - Support external ICC/DCP profiles and deterministic profile selection.
   - Add robust chromatic adaptation and gamut mapping in a documented scene-referred working space.

### P1 — major practical quality features

4. **Pre-demosaic sensor corrections**
   - bad/hot/dead pixels, PDAF pixels, green equilibration, row/column noise, masked-black analysis, lens shading, and camera-specific defect data.

5. **Noise and detail pipeline**
   - camera/ISO noise profiles;
   - raw-domain denoise before demosaic;
   - demosaic-aware chroma cleanup;
   - detail-preserving luma denoise;
   - capture sharpening and output sharpening.

6. **Profile-driven lens corrections**
   - distortion, vignetting, lateral chromatic aberration, crop/scale, and lens database integration.

7. **Color-managed output**
   - display ICC transforms;
   - soft proof and output intent;
   - 16-bit integer and floating-point export;
   - embedded profiles and metadata/orientation preservation.

### P2 — rendering quality and architecture

8. **Scene-referred tone and color-tool redesign**
   - replace bespoke Lightroom-like matching with documented, monotonic, hue-stable operators;
   - use a perceptual/gamut-aware color model for selective color;
   - add curve and color-grading tools with explicit working spaces.

9. **Highlight reconstruction validation and expansion**
   - reference-test the Ansel-derived method;
   - add robust segmentation/inpainting for large clipped regions;
   - validate Bayer and X-Trans separately.

10. **Geometry and format coverage**
    - non-square pixels, Fuji SuperCCD/staggered layouts, linear DNG, full-color and monochrome RAW, multi-shot/pixel-shift files, and supported non-RGB mosaics.

11. **Performance architecture**
    - tiled processing and bounded-memory full-resolution export;
    - compact repeating black-pattern resources rather than a full `R32Float` map;
    - GPU capability fallbacks and CI on Vulkan, Metal, DirectX, and WebGPU targets.

12. **Robustness engineering**
    - metadata fuzzing;
    - corrupt/truncated RAW corpus;
    - property tests for orientation and CFA transforms;
    - GPU validation and shader parsing in CI.

## Verification performed in this environment

Completed:

- source-level review of all Rust/WGSL processing code;
- clean-tree diff review against the uploaded archive;
- NUL-byte and balanced-delimiter scan across all Rust and WGSL files;
- binding inventory check after adding the black-level texture;
- pure-function simulations/checks for black-pattern indexing, orientation, white-level fallback, and matrix behavior;
- added Rust unit tests for the corrected helpers;
- patch whitespace validation with Git.

Not completed here:

- `cargo check`, `cargo test`, or `rustfmt`;
- Naga execution of the existing WGSL parser tests;
- creation of WGPU pipelines on a real adapter;
- LibRaw decode tests against real camera files;
- visual or metric comparison against darktable/Ansel/Adobe exports.

The runtime did not contain a Rust toolchain or cached dependencies, and outbound package installation was unavailable. The changes therefore require a normal Rust development machine or CI before merge.

Recommended acceptance commands:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Then run a real RAW corpus through both Preview and High quality on at least one Vulkan and one Metal/DirectX adapter.

## Primary references

- LibRaw data structures: https://www.libraw.org/docs/API-datastruct-eng.html
- LibRaw C++ API and `COLOR`/black subtraction semantics: https://www.libraw.org/docs/API-CXX.html
- Current LibRaw types: https://github.com/LibRaw/LibRaw/blob/master/libraw/libraw_types.h
- darktable demosaic implementations: https://github.com/darktable-org/darktable/tree/master/src/iop/demosaicing
- darktable GPU kernels: https://github.com/darktable-org/darktable/tree/master/data/kernels
- Ansel image-operation sources: https://github.com/aurelienpierreeng/ansel/tree/master/src/iop
- Adobe Camera Raw overview: https://helpx.adobe.com/camera-raw/using/introduction-camera-raw.html
- Adobe raw/camera profiles: https://helpx.adobe.com/camera-raw/using/adjust-color-rendering-camera-camera.html
- Adobe DNG resources/specification: https://helpx.adobe.com/camera-raw/digital-negative.html
