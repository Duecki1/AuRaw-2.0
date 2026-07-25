# Hybrid revision 8 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 336 passed;
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

Revision 7 replaced the ill-conditioned rational decode immediately below the
upper endpoint with a finite C1 Hermite shoulder. The shoulder begins at an
exactly representable f32 coordinate, matches the rational branch in value and
first derivative, and reaches 32768 scene-linear with zero derivative at encoded
1.0. Its paired encoder uses a bounded bisection inverse only for extreme scene
values. Revision 8 clamps the rational inverse branch to the exact encoded
shoulder coordinate. This prevents float32 rounding at the scene-domain join
from stepping one encoded value into the shoulder and making
decode(encode(y)) decrease as y increases.

A dense float32 reference scans 256 representable scene values on each side of
the join. A second test advances to the first actual encoded transition after
the small quantization plateau and verifies that the decoded response increases
by no more than one unavoidable shoulder-decoder step. Float16 storage is also
finite and monotonic at both checks.

The first global and local Hermite tangent is limited according to the decoder's
actual scene-domain derivative. Both the positive curve and signed negative
extension therefore share a maximum scene slope of 1048576. Float32 references
cover the final 128 encoded values below 1.0, raw first-segment tangents from
-200 through +200, the first eight negative half-float values, -1e-4, and actual
float16 storage. The acceptance checks bound the first eight-step excursion to
0.51 scene units before storage and 32 units after storage.

The ratio-preserving composite master-curve headroom limiter remains unchanged.
Scalar R, G, and B curves retain independent output bounds because they are
intentionally channel-specific.

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

Revision 8 remains the canonical consolidation base, but the unresolved
blockers in `HYBRID_REMEDIATION.md` remain merge gates.
