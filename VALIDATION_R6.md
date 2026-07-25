# Hybrid revision 6 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 332 passed;
- Python compilation for scripts, tests, and regression helpers: passed;
- camera-profile validator: 61/61 passed;
- demosaic validator: 36/36 passed;
- regression corpus metadata/file validation: passed;
- reference-engine contract validation: passed;
- low-tone analytical report generation: passed (2,952 rows);
- Gradle wrapper 8.11.1 integrity validation: passed;
- source-tree/connectivity validation: passed;
- immutable workflow-action pin validation: passed;
- merge-marker and rejected-patch scan: passed.

Revision 6 removes the revision-5 encoded endpoint plateau. All nonnegative
master and channel curve values now use the ordinary capped rational decoder.
The signed zero extension is flattened only while the evaluated decoder is
actually capped at 32768 scene-linear. Float32 references explicitly cross the
cap boundary in descending and ascending directions, test the exact endpoint
and adjacent representable values, and inspect float16 storage. The acceptance
checks reject thousand-unit branch jumps while allowing the unavoidable adjacent
float32 decoder quantization near the asymptote. Global and local curve paths use
the same shared decoder and derivative helper.

The ratio-preserving composite master-curve headroom limiter introduced in
revision 5 is retained. Scalar R, G, and B curves retain independent bounds
because they are intentionally channel-specific.

These tests are analytical/source-policy checks and do not execute WGSL. The
distributed archive removes `.pytest_cache`, `__pycache__`, `.pyc`, and other
generated validation artifacts while preserving executable mode on `gradlew`.

Not performed because the required toolchains/backends are unavailable in this
environment:

- `cargo fmt`, `cargo check`, `cargo clippy`, `cargo test`, docs, or release builds;
- Naga/WGSL compilation;
- Android debug/release builds through the wrapper;
- Vulkan, Metal, DX12, or OpenGL rendering;
- full-pipeline CPU/GPU rendered comparisons or real RAW comparisons.

This revision remains the canonical consolidation base, but the unresolved
blockers in `HYBRID_REMEDIATION.md` remain merge gates.
