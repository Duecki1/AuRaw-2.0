#!/usr/bin/env python3
"""Compare ColorChecker reference/rendered XYZ D50 values with CIEDE2000.

CSV columns:
  patch,reference_x,reference_y,reference_z,rendered_x,rendered_y,rendered_z
Optional:
  illuminant,stage,neutral

XYZ values may use Y=1 or Y=100; both sides must use the same scale per row.
Inputs are expected to be in the same D50-referenced PCS. If a renderer emits
another white point, adapt it to D50 before using this tool so the diagnostic
measures rendering error rather than a reference-space mismatch.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
from collections import defaultdict
from pathlib import Path

D50 = (0.96422, 1.0, 0.82521)


def xyz_to_lab(xyz, white=D50):
    scale = 100.0 if max(abs(v) for v in xyz) > 2.0 else 1.0
    x, y, z = (v / scale for v in xyz)
    xr, yr, zr = x / white[0], y / white[1], z / white[2]
    delta = 6.0 / 29.0
    threshold = delta ** 3
    linear_scale = 1.0 / (3.0 * delta * delta)
    linear_offset = 4.0 / 29.0

    def f(t):
        return t ** (1.0 / 3.0) if t > threshold else linear_scale * t + linear_offset

    fx, fy, fz = f(xr), f(yr), f(zr)
    return (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))


def delta_e_2000(lab1, lab2):
    # Sharma, Wu & Dalal (2005), kL=kC=kH=1.
    L1, a1, b1 = lab1
    L2, a2, b2 = lab2
    C1 = math.hypot(a1, b1)
    C2 = math.hypot(a2, b2)
    Cbar = 0.5 * (C1 + C2)
    Cbar7 = Cbar ** 7
    G = 0.5 * (1.0 - math.sqrt(Cbar7 / (Cbar7 + 25.0 ** 7))) if Cbar else 0.0
    ap1, ap2 = (1.0 + G) * a1, (1.0 + G) * a2
    Cp1, Cp2 = math.hypot(ap1, b1), math.hypot(ap2, b2)

    def hp(ap, b):
        if ap == 0.0 and b == 0.0:
            return 0.0
        h = math.degrees(math.atan2(b, ap))
        return h + 360.0 if h < 0.0 else h

    hp1, hp2 = hp(ap1, b1), hp(ap2, b2)
    dLp = L2 - L1
    dCp = Cp2 - Cp1
    dh = hp2 - hp1
    if Cp1 * Cp2 == 0.0:
        dhp = 0.0
    elif abs(dh) <= 180.0:
        dhp = dh
    elif dh > 180.0:
        dhp = dh - 360.0
    else:
        dhp = dh + 360.0
    dHp = 2.0 * math.sqrt(Cp1 * Cp2) * math.sin(math.radians(dhp / 2.0))

    Lbarp = 0.5 * (L1 + L2)
    Cbarp = 0.5 * (Cp1 + Cp2)
    hsum = hp1 + hp2
    if Cp1 * Cp2 == 0.0:
        hbarp = hsum
    elif abs(hp1 - hp2) <= 180.0:
        hbarp = 0.5 * hsum
    elif hsum < 360.0:
        hbarp = 0.5 * (hsum + 360.0)
    else:
        hbarp = 0.5 * (hsum - 360.0)

    T = (
        1.0
        - 0.17 * math.cos(math.radians(hbarp - 30.0))
        + 0.24 * math.cos(math.radians(2.0 * hbarp))
        + 0.32 * math.cos(math.radians(3.0 * hbarp + 6.0))
        - 0.20 * math.cos(math.radians(4.0 * hbarp - 63.0))
    )
    dtheta = 30.0 * math.exp(-((hbarp - 275.0) / 25.0) ** 2)
    RC = 2.0 * math.sqrt(Cbarp ** 7 / (Cbarp ** 7 + 25.0 ** 7)) if Cbarp else 0.0
    SL = 1.0 + 0.015 * (Lbarp - 50.0) ** 2 / math.sqrt(20.0 + (Lbarp - 50.0) ** 2)
    SC = 1.0 + 0.045 * Cbarp
    SH = 1.0 + 0.015 * Cbarp * T
    RT = -math.sin(math.radians(2.0 * dtheta)) * RC
    x = dLp / SL
    y = dCp / SC
    z = dHp / SH
    return math.sqrt(x * x + y * y + z * z + RT * y * z)


def truthy(value: str | None) -> bool:
    return (value or "").strip().lower() in {"1", "true", "yes", "y", "neutral"}


def load_rows(path: Path):
    rows = []
    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        required = {
            "patch",
            "reference_x", "reference_y", "reference_z",
            "rendered_x", "rendered_y", "rendered_z",
        }
        missing = required.difference(reader.fieldnames or [])
        if missing:
            raise SystemExit(f"missing CSV columns: {', '.join(sorted(missing))}")
        for row in reader:
            ref = tuple(float(row[f"reference_{axis}"]) for axis in "xyz")
            out = tuple(float(row[f"rendered_{axis}"]) for axis in "xyz")
            ref_lab = xyz_to_lab(ref)
            out_lab = xyz_to_lab(out)
            rows.append({
                "patch": row["patch"],
                "illuminant": row.get("illuminant", "unspecified") or "unspecified",
                "stage": row.get("stage", "final") or "final",
                "neutral": truthy(row.get("neutral")),
                "reference_lab": ref_lab,
                "rendered_lab": out_lab,
                "delta_e_2000": delta_e_2000(ref_lab, out_lab),
                "neutral_chroma": math.hypot(out_lab[1], out_lab[2]),
            })
    return rows


def aggregate(rows):
    groups = defaultdict(list)
    for row in rows:
        groups[(row["illuminant"], row["stage"])].append(row)
    output = {}
    for (illuminant, stage), group in sorted(groups.items()):
        errors = [r["delta_e_2000"] for r in group]
        neutrals = [r for r in group if r["neutral"]]
        output[f"{illuminant} / {stage}"] = {
            "patch_count": len(group),
            "mean_delta_e_2000": sum(errors) / len(errors),
            "max_delta_e_2000": max(errors),
            "neutral_patch_count": len(neutrals),
            "mean_neutral_lab_chroma": (
                sum(r["neutral_chroma"] for r in neutrals) / len(neutrals) if neutrals else None
            ),
            "max_neutral_lab_chroma": max((r["neutral_chroma"] for r in neutrals), default=None),
        }
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("csv", type=Path, nargs="?")
    parser.add_argument("--json", type=Path, help="also write machine-readable results")
    parser.add_argument(
        "--self-check", action="store_true",
        help="verify the CIEDE2000 implementation against Sharma et al. test pair 1",
    )
    args = parser.parse_args()
    if args.self_check:
        measured = delta_e_2000((50.0, 2.6772, -79.7751), (50.0, 0.0, -82.7485))
        expected = 2.0425
        error = abs(measured - expected)
        print(f"CIEDE2000 self-check: {measured:.8f} (expected {expected:.4f}, abs error {error:.8g})")
        if error > 5e-5:
            raise SystemExit("CIEDE2000 self-check failed")
        if args.csv is None:
            return
    if args.csv is None:
        parser.error("CSV path is required unless --self-check is used")
    rows = load_rows(args.csv)
    if not rows:
        raise SystemExit("CSV contains no patches")
    summary = aggregate(rows)
    print(json.dumps(summary, indent=2))
    worst = sorted(rows, key=lambda r: r["delta_e_2000"], reverse=True)[:8]
    print("\nWorst patches:")
    for row in worst:
        print(
            f"  {row['illuminant']} / {row['stage']} / {row['patch']}: "
            f"dE00={row['delta_e_2000']:.4f}, C*ab={row['neutral_chroma']:.4f}"
        )
    if args.json:
        args.json.write_text(
            json.dumps({"summary": summary, "patches": rows}, indent=2), encoding="utf-8"
        )


if __name__ == "__main__":
    main()
