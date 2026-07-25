#!/usr/bin/env python3
"""Standalone Process-17 Shadows/Blacks transfer-response analysis.

The model mirrors the analytical shader math for neutral grayscale input and the
project's default sigmoid coefficients. It emits CSV rows requested by the
regression review: scene input/output, effective EV shift, selector weight, and
final display-linear output.
"""
from __future__ import annotations

import argparse
import csv
import math
from pathlib import Path

SCENE_MIDDLE_GREY = 0.1845
INPUTS = [0.00001, 0.00003, 0.0001, 0.0003, 0.001, 0.003, 0.01, 0.03, 0.1, 0.18, 0.5, 1.0, 4.0]
SETTINGS = [-100, -50, -25, 0, 25, 50, 100]
DEFAULT_PERCENTILES = (-8.0, -5.0, 0.0)

# AuRaw/darktable-default sigmoid coefficients in src/pipeline/sigmoid.rs.
SIGMOID_WHITE = 1.0
SIGMOID_LOG2_PAPER_EXPOSURE = -1.4751521
SIGMOID_FILM_FOG = 0.0013843221
SIGMOID_FILM_POWER = 1.4909091
SIGMOID_PAPER_POWER = 1.0


def clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def smoothstep(a: float, b: float, x: float) -> float:
    t = clamp((x-a)/max(b-a, 1e-6), 0.0, 1.0)
    return t*t*(3.0-2.0*t)


def shaped(v: float) -> float:
    n = clamp(v/100.0, -1.0, 1.0)
    m = abs(n)
    return math.copysign(m*(1.45-0.45*m), n) if m else 0.0


def shadow_bounds(p005: float, p05: float, p50: float) -> tuple[float,float,float]:
    p005 = clamp(p005, -15.5, 11.0)
    p05 = max(clamp(p05, -15.25, 11.25), p005+0.25)
    p50 = max(clamp(p50, -15.0, 11.5), p05+0.5)
    lower = clamp(min(p005-0.5, p05-2.5), -13.0, -6.0)
    peak = max(clamp(p05+1.25, -6.0, -2.0), lower+2.5)
    upper = min(max(p50+0.5, peak+3.5), 0.75)
    upper = max(upper, peak+2.5)
    return lower,peak,upper


def shadow_mask(ev: float, b: tuple[float,float,float]) -> float:
    lo,pk,hi=b
    return smoothstep(lo,pk,ev) if ev <= pk else 1.0-smoothstep(pk,hi,ev)


def shadows_scene(y: float, setting: float, p=DEFAULT_PERCENTILES) -> tuple[float,float,float]:
    if y <= 0.0: return y, 0.0, 0.0
    ev=math.log2(y/SCENE_MIDDLE_GREY)
    b=shadow_bounds(*p)
    w=shadow_mask(ev,b)
    a=shaped(setting)
    lo,pk,hi=b
    if a>=0:
        strength=min(a*3.40, 0.64*max(hi-pk,0.25))
    else:
        strength=-min((-a)*3.00,0.64*max(pk-lo,0.25))
    de=strength*w
    return y*2**de, de, w


def sigmoid(y: float) -> float:
    base=SIGMOID_FILM_FOG+max(y,0.0)
    if base <= 0: return 0.0
    log2_f=SIGMOID_FILM_POWER*math.log2(base)
    rlog=log2_f-SIGMOID_LOG2_PAPER_EXPOSURE
    if rlog>=0:
        ratio=1.0/(1.0+2**(-rlog))
    else:
        z=2**rlog; ratio=z/(1.0+z)
    return SIGMOID_WHITE*clamp(ratio,0.0,1.0)**SIGMOID_PAPER_POWER


def blacks_display(y: float, setting: float) -> tuple[float,float,float]:
    pivot=0.15
    if y <= 1e-8 or y>=pivot or setting==0:
        return y,0.0,0.0
    x=clamp(y/pivot,0.0,1.0)
    w=(1.0-x)**2
    a=shaped(setting)
    endpoint=2.60 if a>=0 else 3.10
    de=a*endpoint*w
    return y*2**de,de,w


def rows():
    for control in ("Shadows","Blacks"):
        for setting in SETTINGS:
            for y in INPUTS:
                if control=="Shadows":
                    scene_out,de,w=shadows_scene(y,setting)
                    display_in=sigmoid(scene_out)
                    display_out=display_in
                    op_out=scene_out
                else:
                    scene_out=y
                    display_in=sigmoid(y)
                    display_out,de,w=blacks_display(display_in,setting)
                    op_out=display_out
                yield {
                    "control":control,
                    "setting":setting,
                    "input_scene_luminance":y,
                    "operation_domain_output":op_out,
                    "scene_output_luminance":scene_out,
                    "effective_ev_change":de,
                    "effective_mask_weight":w,
                    "display_domain_input":display_in,
                    "display_domain_output":display_out,
                }


def main() -> None:
    ap=argparse.ArgumentParser()
    ap.add_argument("--csv", type=Path)
    args=ap.parse_args()
    data=list(rows())
    fields=list(data[0])
    if args.csv:
        args.csv.parent.mkdir(parents=True, exist_ok=True)
        with args.csv.open("w",newline="",encoding="utf-8") as f:
            w=csv.DictWriter(f,fieldnames=fields); w.writeheader(); w.writerows(data)
    else:
        w=csv.DictWriter(__import__('sys').stdout,fieldnames=fields); w.writeheader(); w.writerows(data)

if __name__=="__main__": main()
