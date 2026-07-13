# Security and correctness repair report

This revision addresses the reported native-code trust, export seams,
resource-exhaustion, false-green CI, RAW-resize, color-pipeline, and parity
issues.

## Native-code and model trust

- Desktop builds no longer download, extract, or automatically select ONNX
  Runtime native libraries.
- A desktop runtime must be selected locally. Its canonical path and SHA-256
  are persisted. On Linux the selected bytes are copied and hashed in one
  pass, sealed in an anonymous `memfd`, and loaded through that descriptor, so
  pathname replacement cannot change the bytes between verification and
  dynamic loading. A different runtime cannot be activated later in the same
  process.
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
- Android document import enforces the same 2 GB input cap both from provider
  metadata and while streaming, handles zero-progress reads, and removes a
  partial cache file on error. Export startup no longer removes other regular
  files from the shared cache directory.
- Unix RAW paths are passed to LibRaw as their original `OsStr` bytes; invalid
  UTF-8 filenames are no longer replaced by lossy Unicode text.

## Export seams and color correctness

- Final output no longer resizes the CFA sensor mosaic.
- RAW processing runs at native resolution. Export reads the developed,
  post-tone-map display-linear Rec.2020 surface, performs linear-light Lanczos
  resampling, applies the output transform once, and writes sRGB PNG bytes.
- The export tile halo is 232 pixels. It is derived from cumulative support:
  initial and guided highlight reconstruction, the full demosaic chain, local
  effects, Glow, and color-mixer stabilization. It remains aligned to the
  global guide grid.
- Export tone statistics are accumulated from every native-resolution tile
  core before rendering. Halo pixels are excluded, the result is reduced once,
  and no preview proxy participates in export highlight/shadow percentiles.
- Adaptive tone analysis now applies user white balance before the DCP HueSat
  map, matching the rendered camera-profile order.

## Build input trust

- GitHub Actions are pinned to full commit IDs rather than mutable major tags.
- The LibRaw and LibRaw-cmake revisions remain commit-pinned, and their archive
  SHA-256 values are verified before extraction. Cached source directories are
  accepted only when their recorded archive digest matches.

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

## Validation performed in the repaired worktree

- `python3 -m pytest -q`: **66 passed**.
- `python3 scripts/validate_camera_profiles.py`: **60/60 passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo check --locked --all-targets`: passed.
- `cargo test --locked --all-targets`: **61 passed, 0 failed**, including WGSL
  parsing/validation and live GPU render/readback on the available adapter.
- `cargo clippy --locked --all-targets`: completed with no errors; existing
  advisory warnings remain.
- Offline Android Java compilation (`:app:compileDebugJavaWithJavac`): passed.
- `sh -n scripts/build-android-libraw.sh`: passed.

## Local verification limits

- `scripts/check-source-tree.py` is currently blocked by pre-existing generated
  `android/app/src/main/jniLibs/arm64-v8a/*.so` files in this checkout.
- Android packaging was not executed as part of this validation pass.
