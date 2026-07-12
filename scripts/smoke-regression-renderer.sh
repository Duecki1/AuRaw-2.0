#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RENDERER="${AURAW_REGRESSION_RENDERER:-$ROOT/target/debug/auraw-regression-render}"
OUT="${AURAW_REGRESSION_SMOKE_DIR:-$ROOT/target/regression-smoke}"

mkdir -p "$OUT/run-1" "$OUT/run-2"
python3 "$ROOT/scripts/image_regression.py" validate-corpus \
  --manifest "$ROOT/regression/corpus.yaml" --verify-files
python3 "$ROOT/scripts/image_regression.py" validate-reference-engines \
  --config "$ROOT/regression/reference-engines.yaml"

for run in 1 2; do
  for scene in synthetic-bayer-multitarget synthetic-xtrans-multitarget; do
    "$RENDERER" --backend gpu \
      --input "$ROOT/regression/raw/${scene%%-multitarget}.dng" \
      --output "$OUT/run-$run/$scene.npz"
  done
done

python3 "$ROOT/scripts/image_regression.py" determinism \
  --manifest "$ROOT/regression/corpus.yaml" --backend gpu \
  --run-a "$OUT/run-1" --run-b "$OUT/run-2" \
  --max-abs 0 --report "$OUT/determinism.json"

python3 - "$OUT/run-1" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve()
project = root.parents[2]
sys.path.insert(0, str(project / "regression"))
from iqr.io import load_linear_image

for path in sorted(root.glob("*.npz")):
    image = load_linear_image(path, color_space="linear-rec2020-d65")
    if image.rgb.shape != (256, 256, 3):
        raise SystemExit(f"unexpected shape for {path}: {image.rgb.shape}")
    if image.metadata.get("renderer") != "auraw-regression-render":
        raise SystemExit(f"missing renderer metadata in {path}")
    print(f"validated {path.name}: {image.rgb.shape}, {image.rgb.dtype}")
PY
