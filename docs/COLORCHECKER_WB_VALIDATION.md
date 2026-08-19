# AuRaw 2.0 ColorChecker White-Balance Validation Protocol

## Purpose

This protocol separates white-balance errors from camera-profile, tone-curve, gamut-map, highlight, and display-transform errors. It is designed for the same RAW capture to be compared in AuRaw and a mature reference processor without assuming that Lightroom/ACR's private Temperature/Tint implementation is known.

## Capture set

Use a spectrally characterized ColorChecker (measured patch reflectances preferred) under at least: Illuminant A / ~2856 K, ~3200 K, D50, D55, D65, and a high-CCT daylight/shade condition. Include an exposure series with unclipped neutrals and one highlight stress series. Record illuminant SPD when possible. Use the same camera, lens, ISO, exposure, and raw file for each cross-processor comparison.

## Processor setup

For Adobe Camera Raw / Lightroom, darktable, RawTherapee, and AuRaw:

1. Use the same DNG/DCP camera profile when the processors can load it. If they cannot, record the exact profile used and do not interpret patch differences as WB-only error.
2. Disable creative looks, local corrections, saturation/vibrance, sharpening, denoise, vignette, lens color corrections, and automatic tone/contrast. Disable or neutralize profile LookTable / tone curves where the application permits.
3. Match exposure numerically. Disable automatic brightness.
4. Use a linear, wide-gamut output for measurements where possible. D50 XYZ/Lab PCS is preferred for the supplied validation tool.
5. Set WB from a neutral patch, then repeat at explicit target white points / Kelvin values. For Lightroom/ACR, label Temp/Tint behavior **empirical/proprietary behavior — not source verified**.

## Stage captures in AuRaw

For every test, record neutral and patch RGB/XYZ at:

1. post-CFA WB, before demosaic;
2. post-demosaic camera RGB;
3. post camera→XYZ/working transform;
4. post DCP HueSatMap;
5. post scene/tone rendering;
6. post display/output transform.

The first stage whose neutral or patch error exceeds numerical tolerance is the actionable divergence. Do not attribute a later DCP/tone mismatch to white balance.

## Measurements

For each patch save a CSV with columns:

`patch,reference_x,reference_y,reference_z,rendered_x,rendered_y,rendered_z`

Optional columns understood by the workflow are `illuminant`, `stage`, and `neutral`. XYZ values must share the same scale and white point; `tools/colorchecker_wb_validate.py` expects D50 PCS XYZ for Lab/ΔE2000.

Run:

```bash
python tools/colorchecker_wb_validate.py measurements.csv --json diagnostics/colorchecker_results.json
```

The tool reports ΔE2000 per patch, mean/median/max ΔE2000, and neutral Lab chroma. It contains the Sharma et al. CIEDE2000 reference-pair self-check (expected 2.0424597).

## Interpretation

A WB defect is indicated when neutral error is already present immediately after the camera transform and changes systematically with selected white. A camera-profile defect is indicated when camera-neutral behavior is correct but ColorChecker chromatic patches diverge after ColorMatrix/ForwardMatrix/HueSatMap. A tone/gamut defect appears later while pre-tone XYZ remains close.

No single universal ΔE threshold is specified here: camera metamerism, chart spectra, profile provenance, and reference-pipeline differences impose a real floor. The important regression criterion is that the same camera/profile/capture becomes no worse at neutral patches and loses systematic CCT/tint-dependent color error after the WB/profile-state fix.
