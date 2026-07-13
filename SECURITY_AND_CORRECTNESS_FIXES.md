# Security and correctness repair report

This revision addresses the reported native-code trust, export seams,
resource-exhaustion, false-green CI, RAW-resize, color-pipeline, and parity
issues.

## Native-code and model trust

- Desktop builds no longer download, extract, or automatically select ONNX
  Runtime native libraries.
- A desktop runtime must be selected locally. Its canonical path and SHA-256
  are persisted, and the file is rehashed immediately before dynamic loading.
  A different runtime cannot be activated later in the same process.
- The BiRefNet model is pinned to an exact SHA-256 and byte length. Existing
  cache entries are revalidated; downloads are streamed through a hard size
  limit into a unique temporary file, synchronized, verified, and renamed only
  after validation.
- Model inputs and outputs are dimension- and allocation-bounded.

## RAW, profile, and export resource limits

- RAW file size, active dimensions, sensor dimensions, sensor pitch, and sensor
  pixel count are checked before `libraw_unpack` can allocate the image.
- Embedded ICC profiles and oriented RAW buffers have explicit limits and
  fallible reservations. Standalone and embedded DCP containers, individual
  tags, hue-saturation maps, and tone curves are independently bounded before
  allocation.
- Export edge, pixel count, tile size, source-band memory, resampling kernels,
  active vertical rows, and temporary-file lifecycle are bounded. Extreme
  aspect ratios cannot retain a full resized frame.
- Invalid dimensions are blocked in the UI and checked again in the worker.
  Incomplete exports remain in unique `.part` files and are removed on failure;
  only a complete PNG is published to the destination.

## Export seams and color correctness

- Final output no longer resizes the CFA sensor mosaic.
- RAW processing runs at native resolution. Export reads the developed,
  post-tone-map display-linear Rec.2020 surface, performs linear-light Lanczos
  resampling, applies the output transform once, and writes sRGB PNG bytes.
- The export tile halo is 104 pixels. This covers the widest current Glow
  support radius (97 pixels) and remains aligned to the global guide grid,
  preventing cross-tile filter seams.

## CI and parity evidence

- CI runs the complete pytest suite rather than the narrower unittest-discovery
  subset that missed static and regression guards.
- Candidate artifacts are rejected unless backend identity, immutable source
  revision, RAW SHA-256, renderer SHA-256, implementation identity/fingerprint,
  color space, transfer function, and output binding are valid.
- CPU/GPU comparison requires the same source revision but different backend,
  implementation, fingerprint, and renderer-executable hash. The repository
  currently ships only a GPU renderer, so parity is explicitly reported as
  unproven until an independently built CPU renderer satisfies that contract.
- Android CI performs a real LibRaw-enabled native build instead of treating a
  no-LibRaw check as proof that the RAW-enabled application builds.

## Validation performed for this archive

- `python3 -m pytest -q`: **62 passed**.
- `python3 scripts/check-source-tree.py`: passed.
- Python bytecode compilation, shell syntax checks, and TOML parsing: passed.
- Rust formatting check: passed.
- Rust 1.92-compatible `cargo check --locked --all-targets`: passed.
- Rust library and runtime WGSL tests: **49 passed, 0 failed**, including the
  live GPU render/readback integration test on the available adapter.
- `cargo clippy --locked --all-targets`: completed with no errors. Existing
  advisory warnings remain because CI does not promote warnings to errors.

## Local verification limits

- The container did not provide the LibRaw development package, so local Rust
  checks used the project's explicit no-LibRaw build mode. The repaired Linux
  and Android CI jobs install/build LibRaw and exercise the RAW-enabled paths.
- The exact stable `1.92.0` toolchain was unavailable locally; validation used
  `rustc 1.92.0-nightly (2025-09-24)`, Cargo 1.92, matching Clippy, and rustfmt.
- Android packaging was not executed locally.
