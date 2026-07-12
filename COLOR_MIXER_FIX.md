# Perceptual Color Mixer Fix

The eight Lightroom-style Hue, Saturation, and Luminance channels retain their
existing Rust parameters and UI ranges. Processing is no longer performed in
mathematical HSL.

## Processing changes

1. `prepare_adjustment_base` renders global Develop controls into the first
   reused demosaic scratch texture (`rgba16float` preview or `rgba32float`
   high quality).
2. `apply_lightroom_effects` reads that exact developed base, applies local
   Texture/Clarity/Dehaze and global Vibrance/Saturation, and writes the second
   reused scratch texture.
3. `apply_lightroom_adjustments` reads the effects result, applies the
   selective mixer, then runs the existing darktable sigmoid/output transform.
4. Hue selection is computed in OKLab using anchors calibrated from the eight
   named sRGB swatches. Ordinary HSL angles are not reused in OKLab.
5. The selector hue is stabilized with an edge-aware 3x3 Android-preview or
   5x5 desktop/high-quality neighbourhood. Center-pixel RGB detail is never
   blurred.
6. Near-neutral and barely exposed pixels receive a smooth confidence of zero
   instead of an arbitrary hue derived from sensor or demosaic noise.
7. Hue and saturation adjustments are reconstructed at constant OKLab
   lightness and hue, with binary-search chroma compression into positive
   Rec.2020 rather than channel clipping.
8. Luminance is a scalar scene-linear exposure gain, preserving RGB ratios.
9. When all mixer sliders are zero, the second pass returns the source pixel
   exactly and skips the neighbourhood filter.

## Validation

- Both `rgba16float` and `rgba32float` adjustment pipelines compile.
- All 66 assembled WGSL compute-pipeline variants compile through wgpu-native.
- Rust source files parse without syntax errors.
- Synthetic GPU checks cover exact neutral bypass, neutral-color protection,
  smooth selected-color luminance gain, finite hue shifts, and finite gamut-
  mapped saturation.
- Project camera-profile, demosaic, source-tree, and Python regression checks
  pass.
