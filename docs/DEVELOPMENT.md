# Development guide

AuRaw keeps dependency-light repository maintenance and CI validation in the
Rust `xtask` crate, including native build orchestration and icon generation.
NumPy-backed image-regression analysis and synthetic-corpus tooling remain in
`scripts/dev.py`; the minimal pre-Rust CI downloader lives in
`scripts/bootstrap_download.py`.

```sh
cargo xtask --help
python3 scripts/dev.py --help
```

The most common commands are:

```sh
cargo xtask check-all
python3 scripts/dev.py bench
cargo run -p auraw-ui --bin auraw --release
cargo run -p auraw-cli --bin auraw-regression-render -- --help
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
cargo xtask icons
python3 scripts/dev.py corpus
python3 scripts/dev.py smoke-regression
```

## GPU benchmark protocol

Build the regression renderer, then run the workgroup benchmark gate:

```sh
cargo run -p auraw-cli --bin auraw-regression-render --release -- --help
python3 scripts/dev.py bench
```

The benchmark renders both committed CC0 synthetic DNG fixtures for every workgroup shape listed in `benchmarks/gpu-budget.json` (currently 8x8, 16x8, and 16x16). It records one warm-up plus repeated measurements, retains per-scene samples and adapter limits, reports pipeline-creation and render p95 values plus export throughput, and evaluates the versioned guardrails. Use `--dry-run` to validate all fixture/configuration command wiring without a GPU binary.

The renderer's `render_ms` measurement covers canonical GPU rendering and readback, while `pipeline_create_ms` isolates pipeline construction and shader compilation from output serialization. The report also retains whole-process wall time as a reproducible regression signal; native per-pass timestamp queries remain the preferred tool for detailed diagnosis. Peak allocation remains guarded in the Rust pipeline before texture creation; texture-format or pass-count changes should update both the code-side estimate and the benchmark baseline.

## Synthetic regression corpus

`python3 scripts/dev.py corpus` regenerates the compact CC0 Bayer and X-Trans DNG fixtures under `regression/raw/`. A reviewed regeneration must reproduce the hashes in `regression/corpus.yaml`; otherwise update the generator contract, manifest hashes, and baseline review together.

Validate the corpus with:

```sh
python3 scripts/dev.py regression validate-corpus \
  --manifest regression/corpus.yaml --verify-files
```

## Pinned reference processing histories

The YAML files in `regression/profiles/` are audited processing-history contracts. Each is bound to an exact reference-engine version or source revision and SHA-256 pinned by `regression/reference-engines.yaml`.

A contract fixes the active-area crop, orientation, black/white metadata, camera white balance, highlight reconstruction, Bayer/X-Trans demosaic methods, disabled denoise and creative modules, linear D65 Rec.2020 color, and float32 TIFF export. Reference wrappers must apply the contract in the pinned application, retain the application-generated XMP/style with the artifacts, and record the contract path and hash in the renderer manifest.

Validate the contracts with:

```sh
python3 scripts/dev.py regression validate-reference-engines \
  --config regression/reference-engines.yaml
```

## White-balance preset data

`data/wb_presets.json` is a compact, zero-fine-tuning subset of darktable's camera white-balance preset database. It retains maker, model, preset name, and channel coefficients used by AuRaw's preset chooser. The source is darktable's `data/wb_presets.json`; attribution and format documentation also exist in darktable's `src/common/wb_presets.c`.

The data is distributed under the GNU General Public License, version 3 or later, consistently with darktable and AuRaw. The original preset data was developed by darktable and UFRaw contributors.

## Android and release helpers

Dependency-light Android and release checks use `cargo xtask`: `check-all`,
`print-metadata`, `verified-download`, `verify-source-revision`, and
`verify-android-16kb`, `build-android`, `build-android-libraw`,
`build-android-lensfun`, `build-linux`, and `icons`. NumPy-backed regression
workflows remain in `scripts/dev.py`; pre-Rust bootstrap downloads use the
standalone `scripts/bootstrap_download.py` helper.
Gradle and CI consume the same `[workspace.metadata]` contract from the root
`Cargo.toml`.
