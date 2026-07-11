# AuRaw image-quality regression

This directory defines a reproducible image regression layer above the existing shader parser and invariant tests. It does not commit third-party RAW files. The corpus is identified by a manifest and SHA-256 values, while image artifacts are stored separately.

## What is compared

Every renderer produces the same canonical intermediate:

- NPZ container with `rgb` as little-endian float32, H×W×3.
- Scene-linear RGB, normally D65 linear Rec.2020.
- No display tone curve, output sharpening, resizing, or transfer function.
- Optional `valid_mask` for pixels that should not participate.
- JSON metadata embedded as `metadata_json`.

The harness computes linear RMSE/PSNR, CIEDE2000, Scharr edge magnitude and direction error, a chroma-Laplacian zippering proxy, neutral-edge false color, and robust flat-field noise/bias metrics. Highlight ROIs add highlight-specific ΔE and maximum-error gates.

## Corpus policy

Start from `corpus.example.yaml`. The enabled corpus must cover all of these labels:

- Bayer and X-Trans.
- High ISO and intentionally underexposed captures.
- Saturated highlights such as LEDs, specular reflections, and colored clipping.
- Difficult frequencies such as textiles, screens, fences, foliage, and slanted edges.

Prefer paired captures of the same target across Bayer and X-Trans cameras. Record camera/firmware details in `source`, preserve the original file byte-for-byte, and pin its SHA-256. A scene should have at least one ROI aimed at its failure mode. RAW files with restrictive licenses belong in a private artifact store mounted at `regression/raw`, not in Git.

Validate metadata and file hashes:

```sh
cp regression/corpus.example.yaml regression/corpus.yaml
python3 scripts/image_regression.py validate-corpus \
  --manifest regression/corpus.yaml --verify-files
```

## Reference exports

Pin both the application version and the XMP/style history stack. The reference history must disable display-referred operations and export full-resolution, scene-linear, floating-point TIFF in the same RGB space used by the manifest. Keep the XMP and its hash beside the privately stored references.

The CLI runner accepts any command template, including darktable and Ansel. Placeholders are `{raw}`, `{output}`, `{scene}`, `{backend}`, and `{repeat}`. The command must produce the requested output path.

```sh
python3 scripts/image_regression.py render \
  --manifest regression/corpus.yaml \
  --backend darktable \
  --command-template 'darktable-cli {raw} regression/profiles/darktable-linear.xmp {output} --width 0 --height 0 --hq true --upscale false --out-ext tif' \
  --output-root regression/reference-tiff/darktable \
  --extension .tif \
  --version-command 'darktable-cli --version'
```

Ansel uses the same positional input/XMP/output pattern, so the equivalent renderer can use `ansel-cli`. Normalize the exported floating TIFFs in one batch:

```sh
python3 scripts/image_regression.py normalize-corpus \
  --manifest regression/corpus.yaml \
  --input-root regression/reference-tiff/darktable \
  --output-root regression/references/darktable \
  --extension .tif --transfer linear \
  --metadata engine=darktable --metadata version=PINNED_VERSION
```

The single-file `normalize` subcommand is available for ad-hoc imports.

Do not label a normal display TIFF as linear. The XMP/style and export profile are part of the baseline and must be reviewed whenever the reference application changes.

## AuRaw CPU/GPU output contract

The CPU and GPU headless render commands should write the canonical NPZ directly. A temporary TIFF or NPY is acceptable when it is immediately normalized. For GPU tests, use `ProcessingQuality::High` so the scene texture is RGBA32Float before readback, discard alpha, and preserve the scene-linear texture before tone mapping. For CPU tests, use the same raw normalization, white balance, matrix, crop, border policy, and demosaic options.

Render twice to test determinism. The runner pins locale, timezone, thread counts, backend name, and seed:

```sh
python3 scripts/image_regression.py render \
  --manifest regression/corpus.yaml --backend gpu \
  --command-template 'auraw-regression-render --backend gpu --input {raw} --output {output}' \
  --output-root regression/candidates/gpu --repeat 2

python3 scripts/image_regression.py determinism \
  --manifest regression/corpus.yaml --backend gpu \
  --run-a regression/candidates/gpu/run-1 \
  --run-b regression/candidates/gpu/run-2 \
  --max-abs 0 --report regression/reports/gpu-determinism.json
```

Use zero tolerance for a fully deterministic path. If a driver requires a nonzero tolerance, document the exact adapter, backend, driver, and evidence supporting the limit.

## Regression commands

Compare each backend against a pinned darktable or Ansel baseline:

```sh
python3 scripts/image_regression.py compare \
  --manifest regression/corpus.yaml \
  --thresholds regression/thresholds.yaml \
  --reference-root regression/references/darktable \
  --candidate-root regression/candidates/gpu/run-1 \
  --backend gpu --reference-engine darktable \
  --report-dir regression/reports/gpu-vs-darktable
```

Compare CPU and GPU directly:

```sh
python3 scripts/image_regression.py cpu-gpu \
  --manifest regression/corpus.yaml \
  --thresholds regression/thresholds.yaml \
  --cpu-root regression/candidates/cpu/run-1 \
  --gpu-root regression/candidates/gpu/run-1 \
  --report-dir regression/reports/cpu-gpu
```

Each comparison writes `report.json`, `junit.xml`, and `report.html`. CI should retain the render manifest, reports, failing NPZ files, and diagnostic crops. A baseline update must be a reviewed change to RAW hashes, reference renderer version, XMP/style hash, or thresholds; never silently overwrite references from a candidate build.

## Threshold calibration

The checked-in thresholds are safe starting points, not a claim that all cameras should share one tolerance. Collect metric distributions from several known-good runs on at least three machines. Tighten thresholds above the observed upper tail, then add scene-level overrides only when the scene has a documented reason. The direct CPU/GPU limits should remain much tighter than reference-export limits.
