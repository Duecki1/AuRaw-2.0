# Hybrid revision 3 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 325 passed;
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

Revision 3 adds analytical cancellation tests for nonzero signed RGB vectors on
both sides of the zero-luminance plane and a descending-first-segment channel
curve test. These remain mathematical references and source-policy checks; they
do not execute WGSL.

Not performed because the required toolchains/backends are unavailable in this
environment:

- `cargo fmt`, `cargo check`, `cargo clippy`, `cargo test`, docs, or release builds;
- Naga/WGSL compilation;
- Android debug/release builds through the wrapper;
- Vulkan, Metal, DX12, or OpenGL rendering;
- full-pipeline CPU/GPU rendered comparisons or real RAW comparisons.

This revision remains the canonical consolidation base, but the unresolved
blockers in `HYBRID_REMEDIATION.md` remain merge gates.
