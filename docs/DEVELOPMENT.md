# Development guide

AuRaw exposes repository maintenance, validation, build, benchmarking, and regression tooling through one entry point:

```sh
python3 scripts/dev.py --help
```

The most common commands are:

```sh
python3 scripts/dev.py check-all
python3 scripts/dev.py bench --enforce-budget
python3 scripts/dev.py icons
python3 scripts/dev.py corpus
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
python3 scripts/dev.py smoke-regression
```

## GPU benchmark protocol

Build `auraw-regression-render`, then run:

```sh
python3 scripts/dev.py bench --enforce-budget
```

The benchmark renders both committed CC0 synthetic DNG fixtures, records one warm-up plus repeated wall-clock measurements, reports median export throughput and p95 latency, and evaluates the versioned guardrails in `benchmarks/gpu-budget.json`. Use `--dry-run` to validate fixture and command wiring without a GPU binary.

Wall-clock startup includes shader compilation, device creation, rendering, readback, and process overhead. It is a reproducible regression signal, not a substitute for native per-pass timestamp queries. Peak allocation remains guarded in the Rust pipeline before texture creation; texture-format or pass-count changes should update both the code-side estimate and the benchmark baseline.

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

Android dependency builds, APK alignment verification, verified downloads, and clean-source revision checks are subcommands of `scripts/dev.py`. The pinned implementation remains shared by Gradle and CI, so local and automated builds execute the same contracts.
