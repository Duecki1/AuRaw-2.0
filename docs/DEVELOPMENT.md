# Development

Use Rust 1.92 with LibRaw, Lensfun, libclang, and the platform graphics
dependencies.

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- \
  -D warnings -W clippy::perf -W clippy::large_stack_arrays \
  -W clippy::redundant_clone -W unreachable-pub
cargo deny check
cargo run -p auraw-ui --bin auraw --release
```

The CPU brush-raster baseline is a harness-free benchmark (kept independent
of the test suite): `cargo bench -p auraw-core --bench mask_rasterization`.
It reports throughput for positive and erase dabs on a fixed 512x512 raster;
the benchmark does not alter production rasterization or numerical behavior.

## Diagnostics and release helpers

`scripts/generate_licenses.sh` is the canonical reproducible wrapper around
cargo-about. It normalizes generated line endings before updating
`THIRD_PARTY_LICENSES.md`; CI uses the same wrapper and pins cargo-about to
0.9.2, which should also be used for local regeneration.

`tools/colorchecker_wb_validate.py` compares rendered and reference D50 XYZ
ColorChecker patches using CIEDE2000. Run `python3 tools/colorchecker_wb_validate.py
--self-check` for the implementation check, or pass a CSV with the required
`patch`, `reference_x/y/z`, and `rendered_x/y/z` columns. `--json PATH`
additionally writes machine-readable results.

`auraw-wb-diagnostics` is an intentionally separate CLI binary for inspecting
camera white-balance coefficients and the camera-to-working matrix without
starting the UI. Build it with `cargo build -p auraw-cli --bin auraw-wb-diagnostics`,
then run `target/debug/auraw-wb-diagnostics RAW [--dcp PROFILE]
[--temperature K] [--tint T]`.

The workspace crates are application-internal and explicitly set
`publish = false`; their package manifests intentionally retain repository
assets and build-time native contracts rather than pretending to be isolated
crates.

## Dependency duplicates

`cargo deny check bans` is reviewed with all supported target triples. Some
duplicate versions are unavoidable: desktop and Android graphics stacks,
Wayland/winit platform adapters, bindgen's parser toolchain, and framework
transitive dependencies have incompatible version requirements. Workspace
direct dependencies are kept on the versions used by the application, and no
dependency is upgraded solely to silence a duplicate-version warning.
