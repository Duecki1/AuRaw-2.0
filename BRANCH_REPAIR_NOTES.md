# uhhhyea branch regression repairs

Compared against the supplied `main` archive, this repair keeps the hybrid
rendering and export additions while restoring interactive behavior that the new
resource accounting made unavailable.

## Repairs

- Process-wide GPU reservations now count resident pipeline allocations once.
  Each individual pipeline is still validated against its complete persistent,
  temporary, and safety-margin peak, but every live preview no longer reserves
  the same mutually exclusive readback/inpainting peak permanently.
- Zoom detail uses a dedicated interactive mask atlas rather than duplicating the
  full 32-layer main-preview atlas.
- Android detail limits are 960/1152/1280 pixels for Fast/Balanced/High so the
  main, navigation, and detail pipelines fit the 384 MiB resident budget.
- A failed optional navigation/detail inpaint upload discards only that cache and
  schedules it for rebuilding. The main preview update and inpainting result are
  no longer blocked by an optional cache failure.
- Executable permissions were restored on the build, regression, download, and
  source-verification shell scripts that were executable in `main`.

## Validation performed

- Python test suite, including new preview-regression checks.
- Python bytecode compilation for scripts, tests, and regression helpers.
- Camera-profile, demosaic, regression-corpus, reference-engine, Gradle-wrapper,
  source-connectivity, workflow-pin, and merge-marker validation scripts.

Rust, WGSL/Naga, Android, and real GPU rendering still require a machine with the
corresponding toolchains and backends.

## Edit rendering consistency

- Canonicalized every supported sidecar process version to process 17 on load.
- Canonicalized copied and saved adjustment snapshots so an old process marker cannot spread to another image.
- Forced GPU parameter packing to use the current process and explicit scene/display graph even if a stale in-memory marker is encountered.
- Removed slider-dependent process-version opt-ins; touching an unrelated control can no longer change which formulas an image uses.
- Bumped the developed-thumbnail semantic cache salt so previously cached legacy renders are regenerated.

This intentionally changes the appearance of legacy compatibility renders once, after which equal adjustment values use equal processing semantics across all images.
