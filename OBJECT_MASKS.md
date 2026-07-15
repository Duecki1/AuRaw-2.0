# Prompted object masks

AuRaw's Object mask is an interactive, local-only SAM 2.1 workflow. Object
selection uses the same soft, feathered dab brush and cursor as the regular Brush
mask, with independent Size and Feather controls. The user paints through the
middle of the object or object part they want. Once refinement finishes, the
temporary brush stroke, cursor ring, and tool hint are hidden. Drawing again on the same Object component clears the previous mask,
prompts, and runtime correction cache before starting a new selection from
scratch. The generated probability mask can still be combined with the other
local-mask components.

## Runtime flow

1. AuRaw captures the same lens-aligned, unedited canonical RGB source used by
   the content-aware range masks (2048-pixel long edge on desktop and 1600 on
   Android).
2. The foreground centerline is reduced to representative points. The painted
   brush footprint becomes a focus box, and background guard points are placed
   just outside that box so SAM is encouraged to choose the painted object part
   instead of the nearest larger foreground object.
3. A square crop is placed around the brush focus and expanded through 1.5x,
   2.3x, and full-frame fallbacks when the decoded mask reaches the crop boundary.
4. The SAM image encoder runs once for a crop. Its two high-resolution feature
   maps and image embedding are cached in memory.
5. Recalculate can reuse those encoder features and the preceding 256x256 mask
   logits. A fresh canvas stroke intentionally discards that cache and creates
   a new selection instead of correcting the previous one.
6. Candidate masks are scored using model quality, foreground agreement,
   background-guard agreement, focus-box coverage, area outside the painted
   focus, and crop-border contact. Only the component connected to the painted
   foreground centerline is retained, with a soft boundary band preserved.
7. An optional edge-aware bilateral pass refines uncertain pixels against the
   canonical RGB crop. The stronger fine-edge mode is intended for hair and
   fur, but it is not a compositing-grade alpha-matting model.

Encoder features and previous logits are runtime-only. Sidecars store the
editable prompt strokes, settings, and final soft mask, not the large feature
tensors.

## On-demand models

Object selection asks for consent before downloading these Apache-2.0 ONNX
files from `akiyamanx/sam2.1-hiera-tiny-onnx`:

| File | SHA-256 |
| --- | --- |
| `sam2.1_hiera_tiny.encoder.onnx` | `667384d1e686de6828b841ac8a24db0fafa2b3452494225f82eeedac56141230` |
| `sam2.1_hiera_tiny.decoder.onnx` | `c40f5aa7d37b681cd500481a85d44839fd81c93dce1e86271a2c866470d22105` |

Downloads use HTTPS, bounded temporary files, streaming SHA-256 verification,
and atomic publication into the cache only after the digest matches. Invalid
cache entries are removed and downloaded again after user approval.

Desktop cache location:

- `$XDG_CACHE_HOME/auraw/models`, or
- `$HOME/.cache/auraw/models` when `XDG_CACHE_HOME` is unset.

Android stores the files under the app's internal `models` directory.

Desktop inference uses the same user-selected, SHA-256-pinned ONNX Runtime
library as Subject selection. Android uses the existing `ort` XNNPACK setup.
No image data is uploaded for inference.

## Interaction and cache invalidation

- Size and Feather match the controls and soft-dab rendering used by the
  regular Brush mask.
- Releasing the initial stroke starts selection and refinement.
- Once a result is applied, the temporary stroke overlay, brush cursor, and tool
  hint disappear.
- Drawing on the same component again first removes the refined mask and old
  prompts, invalidates cached SAM logits/features for that selection, and then
  starts a clean replacement selection.
- Recalculate reruns the stored prompt without showing it on the canvas.
- Clear selection removes the generated object mask and its prompt.
- Editing exposure, tone, color, or local adjustments does not invalidate the
  encoder cache because inference uses the canonical unedited source.
- Opening another image, changing geometry-affecting source state, replacing
  prompts, or expanding the crop invalidates the relevant runtime cache.
- Worker generations reject stale results when a newer correction was queued.
- On touchscreens, beginning any mask stroke creates a reversible snapshot. If a
  second finger joins for pinch zoom or pan, the pending stroke is cancelled and
  the exact pre-gesture mask state is restored, so no stray first dab remains.
