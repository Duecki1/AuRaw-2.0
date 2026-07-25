# Hybrid revision 4 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 329 passed;
- Python compilation for scripts, tests, and regression helpers: passed;
- camera-profile validator: 61/61 passed;
- demosaic validator: 36/36 passed;
- regression corpus metadata/file validation: passed;
- reference-engine contract validation: passed;
- low-tone analytical report generation: passed;
- Gradle wrapper 8.11.1 integrity validation: passed;
- source-tree/connectivity validation: passed;
- immutable workflow-action pin validation: passed;
- merge-marker and rejected-patch scan: passed.

Revision 4 caps point-curve decoding at 32768 scene-linear, bounds signed curve
outputs to ±60000, and derives global/local zero slopes from the decoder's actual
clamped one-sided derivative. Analytical references cover endpoints at 1.0,
just below 1.0, the legacy 0.999999 boundary, the exact finite decode ceiling,
and descending first segments with negative scene input. These tests do not
execute WGSL.

The distributed archive removes `.pytest_cache`, `__pycache__`, `.pyc`, and
other generated validation artifacts while preserving executable mode on
`gradlew`.

Not performed because the required toolchains/backends are unavailable in this
environment:

- `cargo fmt`, `cargo check`, `cargo clippy`, `cargo test`, docs, or release builds;
- Naga/WGSL compilation;
- Android debug/release builds through the wrapper;
- Vulkan, Metal, DX12, or OpenGL rendering;
- full-pipeline CPU/GPU rendered comparisons or real RAW comparisons.

This revision remains the canonical consolidation base, but the unresolved
blockers in `HYBRID_REMEDIATION.md` remain merge gates.
