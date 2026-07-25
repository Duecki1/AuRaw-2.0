# Hybrid revision 2 validation

Validation performed on 2026-07-25:

- `python3 -m pytest -q`: 322 passed;
- Python compilation for scripts, tests, and regression helpers: passed;
- camera-profile validator: 61/61 passed;
- demosaic validator: 36/36 passed;
- regression corpus metadata/file validation: passed;
- reference-engine contract validation: passed;
- Gradle wrapper 8.11.1 integrity validation: passed;
- source-tree/connectivity validation: passed;
- immutable workflow-action pin validation: passed.

The Python remediation tests are source-policy checks, and the processing tests
are independent mathematical references. They do not compile or execute WGSL.

Not performed because the required toolchains/backends are unavailable in this
environment:

- `cargo fmt`, `cargo check`, `cargo clippy`, `cargo test`, docs, or release builds;
- Naga/WGSL compilation;
- Android debug/release builds through the wrapper;
- Vulkan, Metal, DX12, or OpenGL rendering;
- full-pipeline CPU/GPU rendered comparisons or real RAW comparisons.

This revision is the canonical consolidation base, but the unresolved blockers
in `HYBRID_REMEDIATION.md` remain merge gates.
