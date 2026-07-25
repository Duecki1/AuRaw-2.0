#!/usr/bin/env python3
"""Static reference-invariant checks for AuRaw's demosaic shader graph."""
from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]


def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


checks: list[tuple[str, bool]] = []


def require(name: str, condition: bool) -> None:
    checks.append((name, condition))


p2 = text("src/shaders/pass2.wgsl")
p3 = text("src/shaders/pass3.wgsl")
p4 = text("src/shaders/pass4.wgsl")
xc = text("src/shaders/xtrans_candidate_common.wgsl")
x4 = text("src/shaders/xtrans_pass4.wgsl")
x5 = text("src/shaders/xtrans_pass5.wgsl")
x6 = text("src/shaders/xtrans_pass6.wgsl")
x7 = text("src/shaders/xtrans_pass7.wgsl")
dual = text("src/shaders/dual_demosaic.wgsl")
gpu = text("src/pipeline/gpu.rs")
common = text("src/shaders/common.wgsl")

require("RCD exterior margin is 9 pixels", "const RCD_MARGIN: i32 = 9" in p4)
require("RCD exterior is initialized by PPG", "return ppg_rgb_at(pos)" in p4)
require("RCD VH blend matches interpolatef(VH, H, V)",
        "green = mix(vertical.x, horizontal.x, vh)" in p2)
require("RCD PQ blend matches interpolatef(PQ, Q, P)",
        "return mix(p_est, q_est, pq)" in p3)
require("RCD green-site VH blend matches reference orientation",
        "return g0 + mix(v_est, h_est, vh)" in p4)
require("RCD uses 13x13 frequency-domain chroma support",
        "for (var dy = -6; dy <= 6" in p4 and "bayer_phase2" in p4)
require("RCD dual mask uses radius-2 Gaussian normalization",
        "gaussian5_weight" in p4 and "detail /= 256.0" in p4)

require("Markesteijn-3 keeps a 17-pixel exterior",
        "const MARKESTEIJN3_MARGIN: i32 = 17" in xc)
require("Markesteijn differentiates eight directional candidates",
        "mark_candidate(pos, index)" in x4 and "index < 8u" in x6)
require("Markesteijn homogeneity threshold is 8x minimum derivative",
        "minimum * 8.0" in x5)
require("Markesteijn builds 3x3 maps and 5x5 sums",
        "for (var dy = -1; dy <= 1" in x5 and "mark_homo_sum5" in x6)
require("Markesteijn quenches opposite direction pairs",
        "hm[index + 4u]" in x6)
require("Markesteijn one-eighth maximum cutoff is retained",
        "maximum - floor(maximum / 8.0)" in x6)
require("X-Trans FDC uses a 13x13 carrier window and median cleanup",
        "for (var dy = -6; dy <= 6" in x7 and "xt_median5" in x7)
require("X-Trans dual mask uses radius-2 Gaussian normalization",
        "xt_gaussian5_weight" in x7 and "detail /= 256.0" in x7)

require("Dual demosaic builds a dedicated full-resolution green guide",
        "dual_green_reconstruct" in dual and "dual_green_write" in dual)
require("Dual demosaic builds a dedicated robust RGB buffer",
        "dual_rgb_reconstruct" in dual and "dual_low_write" in dual)
require("Dual low branch uses symmetric gradient support",
        "q0 = pos + vec2<i32>(dx, dy)" in dual and "q1 = pos - vec2<i32>(dx, dy)" in dual)
require("Dual low branch is sensor-noise aware",
        "params.noise_read" in dual and "params.noise_shot" in dual)
require("Dual low branch exports reconstruction confidence",
        "red.confidence" in dual and "green_sample.a" in dual)
require("Bayer finish consumes the independent low buffer",
        "dual_low_read" in p4 and "mix(low.rgb, reference" in p4)
require("X-Trans finish consumes the independent low buffer",
        "xtrans_dual_low_read" in x7 and "mix(low.rgb, reference" in x7)
require("Dual passes are skipped unless Dual mode is enabled",
        "needs_dual_demosaic_passes" in gpu
        and "self.demosaic_dual_start_index" in gpu
        and "if params.needs_dual_demosaic_passes()" in gpu)

for entry in (
    "dual_green_reconstruct",
    "dual_rgb_reconstruct",
    "xtrans_seed",
    "xtrans_markesteijn_pass1",
    "xtrans_markesteijn_pass2",
    "xtrans_markesteijn_pass3",
    "xtrans_markesteijn_derivatives",
    "xtrans_markesteijn_homogeneity",
    "xtrans_markesteijn_accumulate",
    "xtrans_demosaic_finish",
):
    require(f"GPU schedule includes {entry}", f'"{entry}"' in gpu)

for field in ("demosaic_mode", "dual_threshold", "frequency_chroma"):
    require(f"Rust/WGSL uniform contains {field}", field in gpu and field in common)

failed = [name for name, ok in checks if not ok]
for name, ok in checks:
    print(f"{'PASS' if ok else 'FAIL'}: {name}")
print(f"\nSummary: {len(checks) - len(failed)} passed, {len(failed)} failed")
if failed:
    sys.exit(1)
