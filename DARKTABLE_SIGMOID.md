# darktable sigmoid port

AuRaw's final scene-to-display transform is a Rust/WGSL port of the sigmoid module in darktable 5.6.0.

## Ported behavior

- darktable's generalized log-logistic transfer function;
- the exact coefficient construction for contrast, skew, target white, and target black;
- middle gray fixed at `0.1845`;
- per-channel processing with negative-value desaturation and hue/energy preservation;
- RGB-ratio processing with darktable's arithmetic-mean luminance estimate and hyperbolic gamut compression;
- darktable 5.6.0 defaults and parameter ranges.

The Rust coefficient implementation is in `src/pipeline/sigmoid.rs`. The GPU implementation is in `src/shaders/tonemap.wgsl`.

## Pipeline placement

User Exposure and other scene-linear controls run first. The DCP HueSat map, baseline exposure, LookTable, and profile tone curve remain upstream of the view transform. AuRaw's Highlights, Shadows, Whites, and Blacks controls are retained as a separate edge-aware scene-linear exposure-shaping stage immediately before sigmoid. With those four controls at zero, they do not affect the darktable transform.

After sigmoid, the existing ICC output LUT converts display-referred linear Rec.2020 to the selected display/output encoding.

## Scope

The port implements the exact darktable 5.6.0 default working-primary path and both official color-processing methods. darktable's optional v2/v3 custom-primary controls (red/green/blue attenuation and rotation, purity recovery, and selectable base primaries) are not exposed by AuRaw. Their defaults are identity for AuRaw's fixed Rec.2020 working space, so they do not change the default transform.

## Validation

`src/pipeline/sigmoid.rs` contains a default-coefficient vector generated from the darktable 5.6.0 C equations plus curve-invariant tests for black, middle gray, white, and monotonicity. `src/pipeline/gpu.rs` includes WGSL parser and uniform-layout tests when the Rust toolchain is available. The source-tree and Python validation scripts can be run without compiling the application.
