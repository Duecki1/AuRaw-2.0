# AuRaw code audit, preview diagnosis, and quality roadmap

Audit date: 2026-07-12

## Scope and limits

This review covers the supplied Rust/wgpu, WGSL, LibRaw, Android, export, and regression code. I inspected the complete source tree, ran all supplied Python/static checks, and patched defects that could be validated without changing the public editing model.

The container did not contain Rust/Cargo or LibRaw development headers and could not reach package servers, so I could not compile the Rust application, validate wgpu shaders through `cargo test`, launch the UI, or render a real RAW. The archive contains two synthetic DNG fixtures but no actual “starlight” RAW and no checked-in darktable/Ansel output images. Image-quality conclusions that depend on a specific camera/file are therefore identified as hypotheses requiring a real sample.

## Fixes applied

### 1. Desktop preview was accidentally forced to half-float

`src/app.rs` always constructed the interactive preview with `ProcessingQuality::Preview` (`RGBA16Float`), even though the project’s own platform default selects `RGBA32Float` on desktop. Desktop now uses `ProcessingQuality::High`; Android remains on half-float to control memory use.

Impact: fewer precision losses in deep shadows, highlight reconstruction, profile transforms, and repeated local/color operations. This is especially relevant to night/astro photographs.

### 2. Initial RAW rendition was mathematically neutral, unlike the reference applications

Newly opened images and Reset previously used 0 EV exposure with a display curve whose neutral middle-gray slope was 1.0. Modern scene-referred darktable/Ansel defaults deliberately provide a photographic starting rendition instead of a neutral linear preview.

AuRaw now has a separate `ExposureParams::scene_referred_default()` for application rendering:

- +0.7 EV initial exposure.
- 1.5 neutral middle-gray contrast baseline in the display transform.
- `ExposureParams::default()` remains a true neutral state for APIs and regression tests.

This avoids silently changing the meaning of sensor-space `black_point`; darktable’s exposure-module black offset is not equivalent to AuRaw’s pre-demosaic sensor black calibration and was not copied.

### 3. Edits leaked from one photograph into the next

Opening a second RAW reused the full previous `ExposureParams`, including exposure, tone, HSL, clarity, dehaze, CA, and denoise edits. New images now start from the scene-referred rendition while preserving only application-level reconstruction choices (highlight method/settings and demosaic mode/settings).

### 4. Adaptive tone statistics did not analyze the rendered profile signal

The tone histogram included the DCP HueSat map and baseline exposure but omitted the DCP LookTable and profile tone curve. The adaptive endpoints could therefore be calculated from a different signal than the final render. `tone_analysis.wgsl` now includes all fixed DCP rendering stages and still deliberately excludes the live user Exposure slider so bounds do not move while adjusting exposure.

### 5. Saturated display colors were hard-clipped at the ICC LUT boundary

`compress_display_gamut()` existed but was not called. Out-of-range display-linear channels were implicitly clamped when sampling the bounded 3D output LUT, which can create hue shifts in bright saturated colors and LEDs. The output path now performs soft display-gamut compression before the ICC LUT.

This is an interim safeguard, not a substitute for a perceptual, output-profile-aware gamut mapper.

### 6. Full export used half-float global tone analysis

The tiled full-resolution export rendered tiles in `RGBA32Float` but derived its global histogram/curve through an `RGBA16Float` proxy pipeline. The export analysis pipeline now uses `ProcessingQuality::High` as well.

### 7. Static regression checks were strengthened

The camera-profile validator now asserts:

- all fixed DCP stages participate in tone analysis,
- the 1.5 neutral photographic contrast baseline is present,
- display-gamut compression occurs before the ICC LUT.

## Why the no-edit starlight preview looked flat

There are several independent causes.

### Fixed causes

1. **No initial exposure lift.** AuRaw started at 0 EV; Ansel/darktable use an initial scene-referred exposure boost around +0.7 EV.
2. **A flat display slope.** AuRaw’s neutral curve used a middle-gray slope of 1.0; current darktable sigmoid uses 1.5 by default.
3. **Desktop half-float processing.** Dark gradients and very low signals were processed in the lower-precision preview path.
4. **Incomplete DCP-aware histogram.** A camera profile’s LookTable/tone curve could change the final image without changing adaptive bounds.
5. **No baseline colorfulness look.** Ansel enables a “standard colorfulness” Color Balance RGB preset; AuRaw’s saturation and vibrance remain neutral.

The first four are patched. I did not add arbitrary global saturation because the correct solution is a hue-constant, gamut-safe colorfulness stage rather than an HSL saturation bump.

### Largest remaining cause: the custom image-adaptive tone mapper

AuRaw does not actually use darktable sigmoid or Ansel filmic. Its custom percentile mapper blends every image with fixed `-8 EV`/`+4 EV` bounds, forces at least a 5.5-stop range, and constrains the white endpoint to at least +1.5 EV before blending. For a low-key night image, this can reserve a large part of the display range for highlights that barely exist, leaving stars and the sky low on the curve.

Illustrative code-path example only: for percentiles around `p0.5=-12`, `p5=-10`, `p50=-7`, `p95=-3`, `p99.5=-1 EV`, the current formula produces roughly `-10.9 EV` black and `+2.2 EV` white. Even the 99.5th percentile remains over three stops below the white endpoint before the new +0.7 EV default exposure. This is a plausible explanation for a flat low-key preview, but it is not a measurement of the missing starlight file.

**Recommended fix:** port the exact darktable generalized log-logistic sigmoid as the default display transform, including its target black/white, per-channel versus RGB-ratio behavior, hue preservation, and gamut handling. Keep the current percentile auto-level algorithm as an optional “Auto tone” mode rather than the default no-edit rendition. For Ansel matching, add filmic RGB as a second selectable view transform using the pinned Ansel source/history.

### Other remaining differences likely visible in starlight/astro RAWs

- No camera/ISO profiled denoise, pre-demosaic RAW denoise, hot-pixel removal, banding/line-noise correction, or astro-specific denoise.
- The preview proxy downsamples the CFA mosaic before demosaic. It averages same-color photosites into a synthetic CFA and then demosaics that proxy. This is fast but does not reproduce full-resolution demosaic texture, star shape, false color, or noise behavior. Demosaic full-resolution tiles first, then resize for a reference-quality preview.
- No capture sharpening, so fine stars and microcontrast are softer than a tuned reference pipeline.
- No camera exposure-bias compensation.
- No monitor profile auto-detection/wiring, despite ICC transform APIs existing in the GPU layer.
- Camera white balance plus a matrix/DCP is present, but there is no full modern chromatic-adaptation/color-calibration stage or standard-colorfulness rendition.
- White/saturation level selection needs real-camera validation. An incorrect per-plane white point causes early clipping and weak or wrong highlight reconstruction.

## Prioritized implementation roadmap

### P0 — establish a trustworthy comparison target

1. **Add real reference images.** `regression/references` is empty. Generate and check in normalized outputs from the pinned darktable and Ansel histories.
2. **Add a license-clean real RAW corpus.** Include several Bayer and X-Trans cameras; low/base/high ISO; tungsten/daylight/mixed LEDs; skin and ColorChecker; foliage/textile/moire; clipped colored lights; deep shadows; and astrophotography/starlight frames. Keep synthetic fixtures for determinism, not perceptual parity.
3. **Separate two regressions.** Compare a scene-linear intermediate for RAW reconstruction/color characterization, and a display-rendered image for default-look parity. Do not compare only final 8-bit output.
4. **Add a real build CI job.** Run `cargo fmt --check`, Clippy with warnings denied, unit tests, naga WGSL validation, headless wgpu rendering on Mesa software Vulkan, and Android cross-build.

### P1 — default rendition and color correctness

5. **Port exact darktable sigmoid.** Use the upstream generalized log-logistic equations and parameters, not only the 1.5 slope. Add deterministic golden-vector tests against the C/OpenCL implementation.
6. **Add selectable Ansel filmic RGB.** Port its display transform and highlight/color reconstruction behavior as a distinct mode.
7. **Implement camera exposure-bias compensation.** Store the relevant metadata separately from user exposure and expose an opt-out.
8. **Implement modern white balance/color calibration.** Separate camera reference white balance from chromatic adaptation; add CAT16/Bradford-style illuminant adaptation, robust temperature/tint controls, dual-illuminant interpolation validation, and optional chart calibration.
9. **Replace linear-sRGB HSL editing.** Implement hue-constant chroma/vibrance and a perceptual color equalizer in a suitable UCS/OKLab-like space with gamut mapping.
10. **Complete color management.** Auto-load the OS monitor ICC profile; expose working/output profile and intent; support matrix/TRC and LUT/mAB ICC profiles through a mature CMS; wire the existing display/output profile APIs into the application.
11. **Keep the scene pipeline unbounded longer.** Avoid early `map_negative_gamut` desaturation/clipping where possible; repair gamut near creative color operations and the view/output transform.

### P1 — RAW reconstruction quality

12. **Reference-quality preview architecture.** Demosaic full-resolution or zoom-dependent tiles, then downsample with a high-quality filter. Use the proxy only for navigation/temporary feedback. Add a visible HQ state while full-quality tiles replace it.
13. **Hot/dead-pixel correction before demosaic.** Include an optional defect map and automatic neighborhood detection.
14. **RAW-domain denoise before demosaic.** Add signal-aware chroma/luma treatment before interpolation.
15. **Camera/ISO profiled denoise.** Use a Poisson-Gaussian noise model and per-camera/ISO profile database with interpolation; provide wavelet/non-local means modes and mask/detail controls.
16. **Green-channel equilibration and line/banding correction.** Important for high ISO and lifted shadows.
17. **Validate demosaic ports with real data.** RCD and Markesteijn-3 are strong choices, but the current checks mostly validate source structure and synthetic metrics. Add upstream golden cases for false color, zippering, star fields, fine fabric, and green imbalance.
18. **Capture sharpening.** Implement a variance/noise-aware, scale-dependent capture-sharpen stage after demosaic, ideally informed by RAW data and ISO.
19. **Improve highlight reconstruction.** Verify camera white points first; then add an inpaint-opposed/segmentation option and a true multi-scale guided-laplacian path. Keep reconstruction noise continuity for high-ISO images.

### P1 — optics and geometry

20. **Lens correction.** Integrate Lensfun and embedded manufacturer/DNG opcodes for distortion, vignetting, and transverse CA. Keep manual overrides.
21. **RAW chromatic aberration correction.** Add automatic pre-demosaic correction; the current red/blue displacement sliders are manual only.
22. **Complete orientation and geometry support.** Handle all metadata orientations used by supported decoders and add a non-square-pixel resampling stage rather than rejecting such files.

### P2 — local processing and output

23. **Multi-scale tone equalizer/local contrast.** Replace small fixed-neighborhood clarity/texture/dehaze heuristics with edge-aware multi-scale algorithms and halo controls.
24. **Masking.** Drawn, parametric, luminance/chroma, edge-aware refinement, and raster masks are needed to use advanced modules safely.
25. **Output formats and precision.** Add 16-bit PNG/TIFF, floating TIFF/EXR where appropriate, JPEG/JPEG XL/AVIF as desired, embedded output profiles, metadata handling, and high-quality resizing.
26. **Dithering and output sharpening.** Dither low-bit-depth exports and add size-aware output sharpening after final resampling.
27. **Non-destructive state.** Per-image history, undo/redo, sidecars, presets, copy/paste, and stable processing-version migration.

## Specific code risks still open

- `build_proxy()` necessarily changes sampling statistics before demosaic; it should not be treated as a full-quality image path.
- Export tone bounds still come from the preview-sized RAW proxy, so rare small highlights can be missed and output can depend on proxy scale. Use a full-resolution reduced histogram or merge per-tile histograms before final rendering.
- `embedded_camera_icc` is captured but not consumed for input characterization.
- `set_display_icc_profile()`/`set_output_icc_profile()` are not wired to the desktop/Android environment or UI.
- The fallback output is hard-coded sRGB unless a caller supplies a LUT.
- Export is only 8-bit RGBA PNG and has no dithering.
- The HSL mixer works in linear sRGB HSL and is not perceptually uniform or robust for large edits.
- The current display gamut compressor is a simple luminance-axis compression in the working cube, not profile-aware perceptual gamut mapping.
- `linear_max` versus LibRaw `maximum` handling is plausible but must be validated camera-by-camera. Vendor linearity metadata can differ from the true clipping threshold.
- Non-square pixels and orientation codes outside 0/3/5/6 are rejected.
- The archive has no `LICENSE`/`COPYING` file and Cargo package metadata has no `license` field. Add the exact intended SPDX expression and retain upstream copyright/license notices and file-level provenance for copied/converted code.

## Verification performed after patches

Passed:

- `python3 -m py_compile scripts/*.py regression/iqr/*.py tests/*.py`
- `python3 scripts/check-source-tree.py`
- `python3 scripts/validate_camera_profiles.py` — 59/59
- `python3 scripts/validate_demosaic.py` — 26/26
- `python3 -m unittest discover -s tests -v` — 12/12

Not run in this environment:

- `cargo fmt --check`
- `cargo check --locked`
- `cargo clippy`
- Rust unit tests and naga WGSL tests
- headless wgpu regression renderer
- Linux GUI launch
- Android build/install
- darktable/Ansel reference rendering
- real starlight RAW comparison

## Files changed

- `src/app.rs`
- `src/pipeline/basicadj.rs`
- `src/pipeline/export.rs`
- `src/shaders/tone_analysis.wgsl`
- `src/shaders/tonemap.wgsl`
- `scripts/validate_camera_profiles.py`
- `PROCESSING_ARCHITECTURE.md`
- `CAMERA_PROFILE_ENGINE.md`
- this audit document

## Primary upstream references consulted

- darktable sigmoid source: https://github.com/darktable-org/darktable/blob/master/src/iop/sigmoid.c
- darktable processing and module documentation: https://docs.darktable.org/usermanual/development/en/
- darktable demosaic/capture sharpening: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/demosaic/
- darktable highlight reconstruction: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/highlight-reconstruction/
- darktable RAW denoise: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/raw-denoise/
- darktable hot pixels: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/hot-pixels/
- darktable lens correction: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/lens-correction/
- darktable Color Balance RGB: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/color-balance-rgb/
- Ansel transition/default presets: https://ansel.photos/en/doc/from-darktable/
