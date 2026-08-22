#!/usr/bin/env python3
"""Deterministic numerical diagnostics for AuRaw 2.0 WB audit.

This is independent of LibRaw/Rust so it can run in CI or an audit sandbox.
It reproduces the pre-fix AuRaw white locus/tint math and the replacement
CCT+Duv model to expose discontinuity/coupling numerically.
"""
from __future__ import annotations
import csv, json, math, pathlib, re
import numpy as np

ROOT = pathlib.Path(__file__).resolve().parents[1]
RUST = ROOT / "crates/auraw-core/src/pipeline/raw_loader/libraw_loader.rs"
OUT = ROOT / "diagnostics"
OUT.mkdir(exist_ok=True)
MIN_K, MAX_K = 1901.0, 25000.0
MIN_TINT, MAX_TINT, MAX_DUV = 0.135, 2.326, 0.05
D65_XYZ = np.array([0.9504559, 1.0, 1.0890578], float)


def parse_cmf():
    text = RUST.read_text()
    m = re.search(r"const CIE_1931_2DEG_5NM: \[\[f64; 3\]; 81\] = \[(.*?)\n\];", text, re.S)
    if not m:
        raise RuntimeError("CIE table not found")
    vals=[]
    for row in re.findall(r"\[([^\]]+)\]", m.group(1)):
        row=row.split("//")[0]
        nums=[float(x.strip().replace('_','')) for x in row.split(',') if x.strip()]
        if len(nums)==3: vals.append(nums)
    if len(vals)!=81: raise RuntimeError(f"expected 81 CMF rows, found {len(vals)}")
    return np.asarray(vals,float)
CMF=parse_cmf()


def xyz_to_xy(xyz):
    s=float(np.sum(xyz)); return np.array([xyz[0]/s, xyz[1]/s])
def xy_to_xyz(xy):
    x,y=xy; return np.array([x/y,1.0,(1-x-y)/y])
def xyz_to_uv(xyz):
    x,y,z=xyz; d=x+15*y+3*z; return np.array([4*x/d,6*y/d])
def uv_to_xyz(uv):
    u,v=uv; return np.array([3*u/(2*v),1.0,(4-u-10*v)/(2*v)])


def planck_xyz(k):
    k=float(np.clip(k,MIN_K,MAX_K)); c2=0.01438776877
    wl=(380.0+5*np.arange(81))*1e-9
    e=c2/(wl*k)
    rad=1.0/(wl**5*np.expm1(e))
    xyz=(rad[:,None]*CMF).sum(axis=0); return xyz/xyz[1]
def planck_uv(k): return xyz_to_uv(planck_xyz(k))

def frame(k):
    m=1e6/float(np.clip(k,MIN_K,MAX_K)); mn=1e6/MAX_K; mx=1e6/MIN_K
    lo=max(m-1,mn); hi=min(m+1,mx)
    t=planck_uv(1e6/hi)-planck_uv(1e6/lo)
    t=t/np.linalg.norm(t)
    n=np.array([-t[1],t[0]])
    if n[1]<0: n=-n
    return t,n

def temp_duv_xyz(k,duv):
    _,n=frame(k); return uv_to_xyz(planck_uv(k)+n*duv)

def project_cct_duv(xyz):
    target=xyz_to_uv(xyz)
    # dense reciprocal-temperature scan + golden refinement; deterministic
    ms=np.linspace(1e6/MAX_K,1e6/MIN_K,2000)
    us=np.array([planck_uv(1e6/m) for m in ms])
    idx=int(np.argmin(np.sum((us-target)**2,axis=1)))
    lo=ms[max(0,idx-2)]; hi=ms[min(len(ms)-1,idx+2)]
    phi=(math.sqrt(5)-1)/2
    c=hi-(hi-lo)*phi; d=lo+(hi-lo)*phi
    f=lambda mm: float(np.sum((planck_uv(1e6/mm)-target)**2))
    for _ in range(55):
        if f(c)<=f(d):
            hi=d; d=c; c=hi-(hi-lo)*phi
        else:
            lo=c; c=d; d=lo+(hi-lo)*phi
    m=(lo+hi)/2; k=float(np.clip(1e6/m,MIN_K,MAX_K)); base=planck_uv(k); _,n=frame(k)
    duv=float(np.dot(target-base,n)); return k,duv

# Pre-fix AuRaw equations.
def old_planck_xy(k):
    t=float(np.clip(k,1667,25000))
    if t<=4000: x=-0.2661239e9/t**3-0.234358e6/t**2+0.8776956e3/t+0.17991
    else: x=-3.0258469e9/t**3+2.107038e6/t**2+0.2226347e3/t+0.24039
    if t<=2222: y=-1.1063814*x**3-1.3481102*x**2+2.1855583*x-0.2021968
    elif t<=4000: y=-0.9549476*x**3-1.3741859*x**2+2.09137*x-0.1674887
    else: y=3.081758*x**3-5.873387*x**2+3.7511299*x-0.3700148
    return np.array([x,y])
def old_temp_xyz(k):
    t=float(np.clip(k,MIN_K,MAX_K))
    if t<4000: xy=old_planck_xy(t)
    else:
        if t<=7000: x=-4.607e9/t**3+2.9678e6/t**2+0.09911e3/t+0.244063
        else: x=-2.0064e9/t**3+1.9018e6/t**2+0.24748e3/t+0.23704
        y=-3*x*x+2.87*x-0.275; xy=np.array([x,y])
    return xy_to_xyz(xy)
def old_temp_tint_xyz(k,tint):
    xyz=old_temp_xyz(k).copy(); xyz[1]/=float(np.clip(tint,MIN_TINT,MAX_TINT)); return xyz

def tint_to_duv(t):
    t=float(np.clip(t,MIN_TINT,MAX_TINT))
    return ((1-t)/(1-MIN_TINT)*MAX_DUV) if t<=1 else -((t-1)/(MAX_TINT-1)*MAX_DUV)
def duv_to_tint(d):
    d=float(np.clip(d,-MAX_DUV,MAX_DUV))
    return 1-(d/MAX_DUV)*(1-MIN_TINT) if d>=0 else 1+(-d/MAX_DUV)*(MAX_TINT-1)

# 1) 4000-K seam
uv_lo=xyz_to_uv(old_temp_xyz(3999.999)); uv_at=xyz_to_uv(old_temp_xyz(4000.0)); uv_hi=xyz_to_uv(old_temp_xyz(4000.001))
old_seam=float(np.linalg.norm(uv_at-uv_lo)); new_local=float(np.linalg.norm(planck_uv(4000.001)-planck_uv(3999.999)))

# 2) Tint trajectories / true CCT coupling
Ts=[2500,3200,4000,5000,5500,6504,7500,10000]
tints=[MIN_TINT,0.4,1.0,1.6,MAX_TINT]
rows=[]; old_shifts=[]; new_errors=[]
for T in Ts:
    for tint in tints:
        ox=old_temp_tint_xyz(T,tint); ok,od=project_cct_duv(ox)
        nd=tint_to_duv(tint); nx=temp_duv_xyz(T,nd); nk,nd2=project_cct_duv(nx)
        old_shifts.append(abs(ok-T)); new_errors.append(abs(nk-T))
        rows.append({
            'temperature_target_K':T,'tint_coordinate':tint,
            'old_x':xyz_to_xy(ox)[0],'old_y':xyz_to_xy(ox)[1],
            'old_u1960':xyz_to_uv(ox)[0],'old_v1960':xyz_to_uv(ox)[1],
            'old_true_cct_K':ok,'old_true_duv':od,'old_cct_shift_K':ok-T,
            'new_x':xyz_to_xy(nx)[0],'new_y':xyz_to_xy(nx)[1],
            'new_u1960':xyz_to_uv(nx)[0],'new_v1960':xyz_to_uv(nx)[1],
            'new_recovered_cct_K':nk,'new_recovered_duv':nd2,'new_cct_error_K':nk-T,
        })
with (OUT/'wb_chromaticity_grid.csv').open('w',newline='') as f:
    w=csv.DictWriter(f,fieldnames=list(rows[0])); w.writeheader(); w.writerows(rows)

# 3) Roundtrip samples for the replacement coordinate system
roundtrip=[]
for T in [1901,2000,2500,2850,3200,4000,5000,5500,6504,7500,10000,15000,25000]:
    for tint in tints:
        d=tint_to_duv(tint); k2,d2=project_cct_duv(temp_duv_xyz(T,d)); t2=duv_to_tint(d2)
        roundtrip.append((abs(k2-T),abs(t2-tint)))

# 4) D65 in CCT+Duv
D65_CCT,D65_DUV=project_cct_duv(D65_XYZ)

# 5) DNG inverse-temperature weights
mired_weight=lambda cct,a,b: float(np.clip(((1e6/cct)-(1e6/a))/((1e6/b)-(1e6/a)),0,1)) if abs(1/a-1/b)>1e-12 else 0.0
weights={str(T):mired_weight(T,2856.0,6504.0) for T in [2856,3200,4000,5000,6504,8000,10000]}

# 6) Algebraic stale-matrix proof with two synthetic camera transforms.
# T(cct) is a deliberately varying 3x3 characterization. GPU applies diag(W).
T_as=np.array([[1.18,-.12,-.06],[-.08,1.10,-.02],[.01,-.10,1.09]])
T_new=np.array([[1.05,-.03,-.02],[-.02,1.05,-.03],[.00,-.04,1.04]])
W_as=np.array([2.1,1.0,1.55]); W_new=np.array([1.25,1.0,2.15])
M_as=T_as@np.diag(1/W_as); M_new=T_new@np.diag(1/W_new)
s=np.array([.18,.18,.18])
old_out=M_as@np.diag(W_new)@s; new_out=M_new@np.diag(W_new)@s
expected=T_new@s
stale_err=float(np.linalg.norm(old_out-expected)); dynamic_err=float(np.linalg.norm(new_out-expected))

# 7) Picker robustness synthetic per-plane samples.
rng=np.random.default_rng(20260819)
truth=np.array([.24,.40,.18,.395])
def make_plane(mu):
    a=rng.normal(mu,0.004,1000)
    a[:40]=.965 # bright contaminant accepted by old 0.98 threshold
    a[40:80]=.004 # dark contaminant
    return a
planes=[make_plane(x) for x in truth]
old_means=np.array([p[(p>.001)&(p<.98)].mean() for p in planes])
def trimmean(p):
    p=np.sort(p[(p>.003)&(p<.97)]); n=len(p); tr=min(n//10,(n-1)//2); q=p[tr:n-tr]; return q.mean()
new_means=np.array([trimmean(p) for p in planes])
true_g=(truth[1]+truth[3])/2; old_g=(old_means[1]+old_means[3])/2; new_g=(new_means[1]+new_means[3])/2
true_wb=np.array([true_g/truth[0],true_g/truth[1],true_g/truth[2],true_g/truth[3]])
old_wb=np.array([old_g/old_means[0],1,old_g/old_means[2],1]) # old discarded G2 ratio
new_wb=np.array([new_g/new_means[0],new_g/new_means[1],new_g/new_means[2],new_g/new_means[3]])
picker_old_err=float(np.linalg.norm(old_wb-true_wb)); picker_new_err=float(np.linalg.norm(new_wb-true_wb))

summary={
 'old_4000K_uv_jump':old_seam,
 'new_planckian_uv_change_3999_999_to_4000_001':new_local,
 'old_max_true_cct_shift_from_tint_K':max(old_shifts),
 'old_median_true_cct_shift_from_tint_K':float(np.median(old_shifts)),
 'new_max_cct_projection_error_K_grid':max(new_errors),
 'new_roundtrip_max_temperature_error_K':max(x[0] for x in roundtrip),
 'new_roundtrip_max_tint_coordinate_error':max(x[1] for x in roundtrip),
 'D65':{'cct_K':D65_CCT,'duv':D65_DUV,'compat_tint':duv_to_tint(D65_DUV)},
 'dng_profile_weight_2856_to_6504':weights,
 'synthetic_stale_dng_matrix':{'old_output':old_out.tolist(),'new_output':new_out.tolist(),'expected_selected_characterization':expected.tolist(),'old_l2_error':stale_err,'new_l2_error':dynamic_err},
 'picker':{'truth_wb':true_wb.tolist(),'old_wb':old_wb.tolist(),'new_wb':new_wb.tolist(),'old_l2_error':picker_old_err,'new_l2_error':picker_new_err},
}
(OUT/'wb_reference_results.json').write_text(json.dumps(summary,indent=2)+'\n')
print(json.dumps(summary,indent=2))
