#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${AURAW_REGRESSION_MANIFEST:-$ROOT/regression/corpus.yaml}"
THRESHOLDS="${AURAW_REGRESSION_THRESHOLDS:-$ROOT/regression/thresholds.yaml}"
REFERENCE_ENGINES="${AURAW_REFERENCE_ENGINES:-$ROOT/regression/reference-engines.yaml}"
REFERENCE_ENGINE="${AURAW_REFERENCE_ENGINE:-darktable}"
REFERENCE_ROOT="${AURAW_REFERENCE_ROOT:-$ROOT/regression/references/$REFERENCE_ENGINE}"
OUTPUT_ROOT="${AURAW_REGRESSION_OUTPUT_ROOT:-$ROOT/regression/candidates}"
REPORT_ROOT="${AURAW_REGRESSION_REPORT_ROOT:-$ROOT/regression/reports}"
: "${AURAW_CPU_RENDER_COMMAND:?Set AURAW_CPU_RENDER_COMMAND with {raw} and {output} placeholders}"
: "${AURAW_GPU_RENDER_COMMAND:?Set AURAW_GPU_RENDER_COMMAND with {raw} and {output} placeholders}"

python3 "$ROOT/scripts/image_regression.py" validate-corpus \
  --manifest "$MANIFEST" --verify-files
python3 "$ROOT/scripts/image_regression.py" validate-reference-engines \
  --config "$REFERENCE_ENGINES"

python3 "$ROOT/scripts/image_regression.py" render \
  --manifest "$MANIFEST" --backend cpu \
  --command-template "$AURAW_CPU_RENDER_COMMAND" \
  --output-root "$OUTPUT_ROOT/cpu" --repeat 2

python3 "$ROOT/scripts/image_regression.py" render \
  --manifest "$MANIFEST" --backend gpu \
  --command-template "$AURAW_GPU_RENDER_COMMAND" \
  --output-root "$OUTPUT_ROOT/gpu" --repeat 2

python3 "$ROOT/scripts/image_regression.py" determinism \
  --manifest "$MANIFEST" --backend cpu \
  --run-a "$OUTPUT_ROOT/cpu/run-1" --run-b "$OUTPUT_ROOT/cpu/run-2" \
  --max-abs "${AURAW_CPU_DETERMINISM_MAX_ABS:-0}" \
  --report "$REPORT_ROOT/cpu-determinism.json"

python3 "$ROOT/scripts/image_regression.py" determinism \
  --manifest "$MANIFEST" --backend gpu \
  --run-a "$OUTPUT_ROOT/gpu/run-1" --run-b "$OUTPUT_ROOT/gpu/run-2" \
  --max-abs "${AURAW_GPU_DETERMINISM_MAX_ABS:-0}" \
  --report "$REPORT_ROOT/gpu-determinism.json"

for backend in cpu gpu; do
  python3 "$ROOT/scripts/image_regression.py" compare \
    --manifest "$MANIFEST" --thresholds "$THRESHOLDS" \
    --reference-root "$REFERENCE_ROOT" \
    --candidate-root "$OUTPUT_ROOT/$backend/run-1" \
    --backend "$backend" --reference-engine "$REFERENCE_ENGINE" \
    --reference-engines "$REFERENCE_ENGINES" \
    --report-dir "$REPORT_ROOT/$backend-vs-$REFERENCE_ENGINE"
done

python3 "$ROOT/scripts/image_regression.py" cpu-gpu \
  --manifest "$MANIFEST" --thresholds "$THRESHOLDS" \
  --cpu-root "$OUTPUT_ROOT/cpu/run-1" \
  --gpu-root "$OUTPUT_ROOT/gpu/run-1" \
  --report-dir "$REPORT_ROOT/cpu-gpu"
