# Glow and Vignette

The creative effects run after the Lightroom-style global and local develop
controls and before the perceptual Color Mixer/display transform.

## Glow

Glow is a scene-linear, highlight-aware bloom rather than a whole-image blur.
The shader extracts bright-source energy with a soft perceptual threshold,
rejects black and low-light regions, preserves the source color ratio with a
small warm bias, and combines two edge-safe multi-scale à-trous gathers.
Highlight-core protection prevents strong sources from washing themselves out.

Standard mode exposes **Amount**. Expert mode additionally exposes:

- **Radius** — changes the spatial scale of the near and far bloom lobes.
- **Threshold** — changes which luminance range emits glow.

An Amount of zero follows an exact bypass path.

## Vignette

The post-crop vignette is evaluated in full-image coordinates so its center and
shape are stable for previews and tiled rendering.

- **Amount** — darkens negative values and lightens positive values.
- **Midpoint** — sets where the edge transition begins.
- **Roundness** — moves between a frame-following rounded rectangle, an image
  ellipse, and a pixel-circular vignette.
- **Feather** — controls the transition softness.
- **Highlights** — restores bright detail inside a dark vignette.

The effect applies a scalar scene-linear gain, which preserves hue and channel
ratios. The image center remains neutral for the default geometry.

## Pipeline order

1. Global Light, tone curve, and Color controls
2. Texture, Clarity, and Dehaze
3. Glow and Vignette
4. Perceptual Color Mixer
5. Darktable-derived display transform
