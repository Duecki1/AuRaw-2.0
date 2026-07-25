# Hybrid revision 5 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 331 passed;
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

Revision 5 uniformly scales extreme composite master-curve RGB triplets instead
of independently clipping channels. The scalar channel curves retain their
intentional component bounds. The decoder now treats the final 1e-6 encoded
domain as a flat endpoint plateau, and float32 references cover the exact
ceiling, one and two representable values below it, the smallest signed
rgba16float subnormal, and a representative -1e-4 input. These tests are
analytical/source-policy checks and do not execute WGSL.

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
