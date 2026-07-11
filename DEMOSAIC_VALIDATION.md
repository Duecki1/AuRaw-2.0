# Demosaic implementation and validation

## Reference baseline

The implementation was reviewed against darktable release 5.6.0, primarily
`src/iop/demosaicing/rcd.c`, `xtrans.c`, `dual.c`, and their OpenCL kernels.
The source pin matters because interpolation constants, padding, and GPU staging
can change independently of the user-facing algorithm names.

## Bayer RCD

The four compute stages retain the darktable RCD sequence:

1. Three-sample vertical/horizontal high-pass discrimination and the RCD low-pass guide.
2. Ratio-corrected green at red/blue sites and P/Q diagonal discrimination.
3. Opposite red/blue interpolation through diagonal colour differences.
4. Red/blue colour-difference interpolation at green sites and scene output.

The complete image is initialized with PPG-style interpolation. RCD replaces it
only in the interior where all source samples are available; the outer nine
pixels remain the border fallback. The VH and P/Q blend orientation is tested
against darktable's `interpolatef` definition.

## Markesteijn 3-pass for X-Trans

The X-Trans graph dispatches seed, three refinement passes, eight-direction
perceptual derivative analysis, 3×3 homogeneity construction, 5×5 homogeneity
summing, opposite-direction quenching, candidate accumulation, and final mode
processing. The Markesteijn-3 exterior margin is 17 pixels.

Eight directional RGB candidates are reconstructed on demand from the refined
base when derivative or accumulation stages require them. This preserves the
directional selection graph without allocating eight persistent full-frame RGB
textures. Measured CFA components are restored after every candidate or border
interpolation.

## Optional modes

- **Frequency-domain chroma:** 13×13 apodized carrier analysis. Bayer evaluates
  the three period-two CFA carriers; X-Trans evaluates period-six horizontal,
  vertical, and diagonal carriers and applies five-sample median cleanup.
  Luminance comes from the high-detail reference result.
- **Dual demosaic:** blends the high-detail result with a low-detail CFA
  interpolation using a Scharr detail response blurred by the separable
  `[1, 4, 6, 4, 1]` radius-two Gaussian. The threshold mapping is
  `0.005 * threshold^1.1`.

## Automated checks performed in this package

- `scripts/validate_demosaic.py`: 26 source and scheduling invariants.
- Rust unit tests: mode values/defaults, WGSL Naga parse/validation, dispatched
  entry points, border constants, interpolation orientation, FDC support, and
  dual-mask normalization.
- All assembled WGSL programs were parsed during this audit, including both
  RCD and all seven X-Trans source groups.
- A Rust syntax comparison against the untouched archive found no new syntax
  errors. The parser's remaining `raw` keyword findings are identical in the
  original project and are an edition mismatch in that third-party parser.

## Remaining runtime validation

The audit environment did not contain a Rust toolchain, GPU adapter, or a
licensed/pinned Bayer and X-Trans RAW corpus, so `cargo test` and image-output
regression renders could not be executed here. Before release, run:

```text
python3 scripts/validate_demosaic.py
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Then compare scene-linear outputs and diagnostic crops against darktable 5.6 on
at least one Bayer and one X-Trans camera, including borders, fabrics, foliage,
resolution charts, saturated highlights, and high-ISO noise.
