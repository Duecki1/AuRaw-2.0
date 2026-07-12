# Lightroom-style controls implementation

This revision reorganizes the editing pipeline into a predictable Lightroom-style order while retaining the existing darktable-derived scene-to-display sigmoid as the final view transform.

## Control order

1. **Light**
   - Exposure
   - Contrast
   - Highlights
   - Shadows
   - Whites
   - Blacks
2. **Tone Curve**
3. **Color**
   - Temperature
   - Tint
   - Vibrance
   - Saturation
4. Effects, Color Mixer, Advanced Rendering, and Raw controls

## Processing order

The GPU path now applies controls in this order:

1. Camera-space reconstruction and working-space conversion
2. Temperature/tint chromatic adaptation
3. DCP profile Hue/Sat map and default exposure
4. DCP look table / profile tone curve
5. User exposure
6. Highlights, shadows, whites, blacks, and contrast
7. User point tone curve
8. Texture, clarity, and dehaze
9. Vibrance and saturation
10. HSL mixer
11. Darktable-style sigmoid display transform
12. Output LUT / display conversion

## Tone curve

The point curve contains up to eight ordered control points. It uses monotone cubic Hermite interpolation so points do not create unwanted ringing or overshoot. The curve is evaluated through a reversible scene-luminance shaper, which keeps a straight diagonal curve as an exact no-op while still allowing the curve to affect HDR scene values.

Interaction:

- Drag a point to edit it.
- Double-click inside the graph to add a point.
- Right-click an interior point to remove it.
- Use **Reset Curve** to restore the diagonal.

## Color controls

- **Temperature/Tint** use a Bradford-style chromatic adaptation in XYZ/LMS rather than RGB channel offsets.
- **Vibrance** operates on perceptual OKLab chroma, gives less gain to already-saturated colors, and reduces the effect near common skin-hue regions.
- **Saturation** also operates on OKLab chroma.
- Out-of-gamut results are pulled back toward neutral before the display transform.

## Validation performed for this revision

- Source-tree validation: passed
- Camera-profile/pipeline validation: 60/60 passed
- Demosaic validation: 26/26 passed
- Python regression suite: 12 passed
- Rust syntax parsing: all 24 Rust source files passed
- WGSL validation: all 15 assembled shader modules were accepted by wgpu/Vulkan
- Numerical checks:
  - OKLab round trip maximum error: approximately 7.7e-7
  - Linear point curve identity maximum error: approximately 2.2e-16

A full Cargo/LibRaw application build was not run in the editing environment because a Rust toolchain was unavailable. Rebuild the desktop application normally before release. The stale prebuilt Android `.so` files were removed deliberately; run the Android build scripts so the native libraries contain this revision.

## Quality and parity note

The controls now use robust scene-referred/perceptual implementations and preserve the existing darktable-derived display transform. Exact pixel-for-pixel Lightroom parity cannot be guaranteed because Lightroom's internal algorithms are proprietary. Objective parity should be measured with a checked-in corpus of real RAW files and reference exports from the target Lightroom and darktable versions. The existing regression harness is ready for those references.
