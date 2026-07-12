# Effects, Blacks, and Expert UI Fix

## Effects processing

The old Texture and Clarity implementation compared a fully developed center
pixel with neighboring pixels reconstructed from an earlier scene stage. That
stage mismatch produced a low-frequency luminance residual, so the controls
behaved like brightness sliders.

The output pipeline now has three full-precision stages:

1. `prepare_adjustment_base`: profile rendering, exposure, Light controls,
   contrast, and point curve into `tex1`.
2. `apply_lightroom_effects`: Texture, Clarity, Dehaze, Vibrance, and Saturation
   from `tex1` into `tex2`.
3. `apply_lightroom_adjustments`: perceptual Color Mixer and display rendering
   from `tex2` into the preview/output texture.

Texture uses a 3x3 edge-aware fine-detail residual with low-signal noise
thresholding. Clarity uses a wider 5x5 B3-spline à-trous selector with spaced
samples for genuine mid-scale contrast. Both operate in log luminance and
reconstruct through a scalar scene-linear gain, preserving RGB ratios. Dehaze
uses a local dark channel, neutral airlight, and bounded transmission model.

## Blacks

The Blacks mask now extends through the useful lower tonal range instead of
being confined close to the darkest five percent. Its endpoint strength was
increased while the mask still fades out before the image median, keeping it
separate from Shadows.

## Interface

- Expert mode is in Settings and defaults to off.
- Advanced Rendering, RAW/demosaic controls, and highlight reconstruction are
  only visible in Expert mode.
- The standard Develop interface retains the Lightroom-like panels.
- The default/reset point curve now contains only the two endpoints.

## Validation

- 25 Python regression tests pass, including complete UI-to-GPU slider wiring.
- 60 camera-profile checks pass.
- 26 demosaic checks pass.
- All 66 assembled WGSL compute-pipeline variants compile in both 16-bit and
  32-bit working formats.
- Rust source parses without syntax errors.
- Manual Rust-equivalent bind-group layouts compile for all three adjustment
  stages.
- Headless GPU behavior checks confirm exact flat-field neutrality for Texture
  and Clarity and directional detail/veil responses.
