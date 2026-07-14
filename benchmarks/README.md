# GPU benchmark protocol

Build `auraw-regression-render`, then run:

```sh
python3 scripts/benchmark-regression-renderer.py --enforce-budget
```

The runner renders both committed CC0 synthetic DNG fixtures, records one warm-up plus repeated wall-clock measurements, reports median export throughput and p95 latency, and evaluates the versioned guardrails in `gpu-budget.json`. `--dry-run` validates the fixture/command wiring without requiring a GPU binary.

Wall-clock startup includes shader compilation, device creation, rendering, readback, and process overhead. It is a reproducible regression signal, not a substitute for native per-pass timestamp queries. Peak allocation is separately guarded in the Rust pipeline before texture creation; texture format and pass-count changes should update both the code-side estimate and this benchmark baseline.
