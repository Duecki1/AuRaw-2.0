# Hybrid revision 8 merge validation

This archive uses `AuRaw-2.0-uhhhyea(3)` as the base and selectively
ports/reconciles the non-regressive fixes from `AuRaw-2.0-hmmokay(3)`.

## Completed checks

- `python3 -m pytest -q`: **336 passed**
- Python compilation for scripts, tests, and regression helpers: **passed**
- `python3 scripts/validate_camera_profiles.py`: **61/61 passed**
- `python3 scripts/validate_demosaic.py`: **36/36 passed**
- regression corpus metadata/file validation: **passed**
- reference-engine contract validation: **passed**
- low-tone analytical report generation: **passed**
- Gradle wrapper 8.11.1 integrity: **verified**
- source-tree connectivity: **passed**
- workflow action pin validation: **passed**
- merge-marker and rejected-patch scan: **passed**

The curve-continuity tests include signed opponent vectors that cross the
zero-luminance plane without shrinking to RGB zero, descending channel segments,
and a finite C1 shoulder across the final 128 encoded f32 values below 1.0. They
exercise realizable first-segment tangents through ±200, the first eight negative
half-float inputs, a -1e-4 input, actual float16 storage, the shared global/local
endpoint-tangent limiter, and bounded adjacent output steps. The composed
float32 encoder/decoder is additionally scanned across 256 representable scene
values on each side of the shoulder join, then through its first actual encoded
transition. Both decoded float32 output and float16 storage are finite and
monotonic; the first transition is limited to one unavoidable shoulder decoder
step. Ratio preservation under extreme composite headroom limiting is retained.
These are analytical references and source-policy checks; they do not execute
WGSL.

## Required before merge

A Rust toolchain and the required Android/GPU backends were unavailable, so the
following still need to run in CI or on supported development machines:

- `cargo fmt --check`
- `cargo check --all-targets --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- Rust documentation and release builds with LibRaw and Lensfun
- Naga/WGPU shader compilation and validation
- Android debug and release builds through the checked-in wrapper
- Vulkan, Metal, DX12, and OpenGL rendering
- full-pipeline CPU/GPU preview/export comparisons and real RAW comparisons

The remaining product and release blockers are listed in
`HYBRID_REMEDIATION.md`. Passing the static and analytical checks above does not
by itself prove rendered image quality or merge readiness.
